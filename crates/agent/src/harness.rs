//! The agent loop: send the conversation to the model, run the tools it asks
//! for, return the results, and repeat until it answers or a limit is hit.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use crate::llm::{LlmClient, Message, Request, Response, ToolUse, Usage};
use crate::tool::{Approver, Decision, Policy, ToolRegistry};

/// Tool output longer than this is cut before it goes back to the model, so
/// one large result can't use up the context window.
pub const MAX_TOOL_RESULT_CHARS: usize = 20_000;

pub struct Config {
    pub model: String,
    pub max_tokens: u32,
    /// Model calls allowed per user message.
    pub max_turns: usize,
    pub system: String,
    pub policy: Policy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The model finished its answer.
    Done,
    /// The model was still calling tools when `max_turns` ran out.
    TurnLimit,
    /// The model stopped for another reason, such as `max_tokens` or `refusal`.
    Stopped(String),
}

pub struct Reply {
    pub text: String,
    pub outcome: Outcome,
}

pub struct Agent {
    llm: Arc<dyn LlmClient>,
    tools: ToolRegistry,
    approver: Arc<dyn Approver>,
    config: Config,
    messages: Vec<Message>,
    usage: Usage,
    transcript: Option<BufWriter<File>>,
}

impl Agent {
    pub fn new(
        llm: Arc<dyn LlmClient>,
        tools: ToolRegistry,
        approver: Arc<dyn Approver>,
        config: Config,
    ) -> Self {
        Self {
            llm,
            tools,
            approver,
            config,
            messages: Vec::new(),
            usage: Usage::default(),
            transcript: None,
        }
    }

    /// Appends every model response and tool call to a JSONL file.
    pub fn with_transcript(mut self, path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create transcript {}", path.display()))?;
        self.transcript = Some(BufWriter::new(file));
        Ok(self)
    }

    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    fn log(&mut self, event: Value) {
        if let Some(w) = self.transcript.as_mut() {
            let mut event = event;
            event["at"] = json!(chrono::Utc::now().to_rfc3339());
            let _ = writeln!(w, "{event}");
            let _ = w.flush();
        }
    }

    /// Sends one user message and runs the loop until the model answers.
    /// The conversation is kept, so later calls continue it.
    pub async fn send(&mut self, user_text: &str) -> Result<Reply> {
        self.log(json!({ "event": "user", "text": user_text }));
        let start_len = self.messages.len();
        self.messages.push(Message::user_text(user_text));

        let mut texts = Vec::new();
        for _ in 0..self.config.max_turns {
            let request = Request {
                model: self.config.model.clone(),
                max_tokens: self.config.max_tokens,
                system: self.config.system.clone(),
                tools: self.tools.specs(),
                messages: self.messages.clone(),
            };
            let response = match self.llm.complete(&request).await {
                Ok(r) => r,
                Err(e) => {
                    // Roll back this exchange so the conversation stays
                    // valid if the caller tries again.
                    self.messages.truncate(start_len);
                    return Err(e);
                }
            };
            self.usage.add(&response.usage);
            self.log(json!({ "event": "response", "response": response }));

            let text = response.text();
            if !text.is_empty() {
                texts.push(text);
            }
            self.messages.push(Message {
                role: "assistant".into(),
                content: response.content.clone(),
            });

            let uses = response.tool_uses();
            match response.stop_reason.as_deref() {
                Some("tool_use") if !uses.is_empty() => {
                    let results = self.run_tools(&uses).await;
                    self.messages.push(Message { role: "user".into(), content: results });
                }
                Some("pause_turn") => {
                    // A server-side tool paused a long turn; send it back to continue.
                }
                Some("end_turn") | Some("stop_sequence") | None => {
                    return Ok(Reply { text: texts.join("\n\n"), outcome: Outcome::Done });
                }
                Some(other) => {
                    let other = other.to_string();
                    if !uses.is_empty() {
                        // Every tool_use must be answered before the next user message.
                        let results = self.skipped_results(&uses, "not run: response was cut off");
                        self.messages.push(Message { role: "user".into(), content: results });
                    }
                    return Ok(Reply { text: texts.join("\n\n"), outcome: Outcome::Stopped(other) });
                }
            }
        }

        // Close any tool calls still open so the next message is valid.
        if let Some(last) = self.messages.last() {
            if last.role == "user" {
                self.messages.push(Message {
                    role: "assistant".into(),
                    content: vec![json!({ "type": "text", "text": "(stopped: turn limit reached)" })],
                });
            }
        }
        Ok(Reply { text: texts.join("\n\n"), outcome: Outcome::TurnLimit })
    }

    fn skipped_results(&self, uses: &[ToolUse], reason: &str) -> Vec<Value> {
        uses.iter()
            .map(|u| tool_result(&u.id, &json!({ "error": reason }), true))
            .collect()
    }

    async fn run_tools(&mut self, uses: &[ToolUse]) -> Vec<Value> {
        let mut results = Vec::with_capacity(uses.len());
        for u in uses {
            let (output, is_error) = self.run_tool(u).await;
            self.log(json!({
                "event": "tool",
                "name": u.name,
                "input": u.input,
                "is_error": is_error,
                "output": output,
            }));
            results.push(tool_result(&u.id, &output, is_error));
        }
        results
    }

    async fn run_tool(&self, u: &ToolUse) -> (Value, bool) {
        let Some(tool) = self.tools.get(&u.name) else {
            return (json!({ "error": format!("unknown tool '{}'", u.name) }), true);
        };
        if self.config.policy.needs_approval(tool.risk())
            && self.approver.approve(&u.name, tool.risk(), &u.input).await == Decision::Deny
        {
            return (json!({ "error": "the operator declined this tool call" }), true);
        }
        tracing::info!(tool = %u.name, "Running tool");
        match tool.call(u.input.clone()).await {
            Ok(v) => (v, false),
            Err(e) => (json!({ "error": format!("{e:#}") }), true),
        }
    }
}

fn tool_result(id: &str, output: &Value, is_error: bool) -> Value {
    let mut text = match output {
        Value::String(s) => s.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    if text.len() > MAX_TOOL_RESULT_CHARS {
        let mut end = MAX_TOOL_RESULT_CHARS;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("… [truncated]");
    }
    json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": text,
        "is_error": is_error,
    })
}

/// Used by tests and dry runs: replays canned responses in order.
pub struct ScriptedLlm {
    responses: std::sync::Mutex<std::collections::VecDeque<Response>>,
    pub requests: std::sync::Mutex<Vec<Request>>,
}

impl ScriptedLlm {
    pub fn new(responses: Vec<Response>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.into()),
            requests: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for ScriptedLlm {
    async fn complete(&self, request: &Request) -> Result<Response> {
        self.requests.lock().unwrap().push(request.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .context("scripted LLM ran out of responses")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{DenyApprover, Risk, Tool};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Echo {
        risk: Risk,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echoes"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object" })
        }
        fn risk(&self) -> Risk {
            self.risk
        }
        async fn call(&self, input: Value) -> Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if input.get("fail").is_some() {
                anyhow::bail!("boom");
            }
            Ok(json!({ "echo": input }))
        }
    }

    fn tool_call(id: &str, input: Value) -> Response {
        serde_json::from_value(json!({
            "content": [{ "type": "tool_use", "id": id, "name": "echo", "input": input }],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 10, "output_tokens": 2 }
        }))
        .unwrap()
    }

    fn answer(text: &str) -> Response {
        serde_json::from_value(json!({
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 20, "output_tokens": 5 }
        }))
        .unwrap()
    }

    fn agent(llm: Arc<ScriptedLlm>, risk: Risk, calls: Arc<AtomicUsize>, max_turns: usize) -> Agent {
        let mut tools = ToolRegistry::default();
        tools.register(Echo { risk, calls });
        Agent::new(
            llm,
            tools,
            Arc::new(DenyApprover),
            Config {
                model: "test".into(),
                max_tokens: 100,
                max_turns,
                system: "sys".into(),
                policy: Policy { auto_approve_writes: true },
            },
        )
    }

    fn last_tool_result(llm: &ScriptedLlm) -> Value {
        let requests = llm.requests.lock().unwrap();
        let msgs = &requests.last().unwrap().messages;
        msgs.last().unwrap().content[0].clone()
    }

    #[tokio::test]
    async fn runs_tool_and_returns_answer() {
        let llm = Arc::new(ScriptedLlm::new(vec![tool_call("t1", json!({ "x": 1 })), answer("done")]));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut a = agent(llm.clone(), Risk::Read, calls.clone(), 5);

        let reply = a.send("hi").await.unwrap();
        assert_eq!(reply.outcome, Outcome::Done);
        assert_eq!(reply.text, "done");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let result = last_tool_result(&llm);
        assert_eq!(result["type"], "tool_result");
        assert_eq!(result["tool_use_id"], "t1");
        assert_eq!(result["is_error"], false);
        assert!(result["content"].as_str().unwrap().contains("\"x\":1"));
        assert_eq!(a.usage().input_tokens, 30);
    }

    #[tokio::test]
    async fn tool_errors_go_back_to_the_model() {
        let llm = Arc::new(ScriptedLlm::new(vec![tool_call("t1", json!({ "fail": true })), answer("ok")]));
        let mut a = agent(llm.clone(), Risk::Read, Arc::new(AtomicUsize::new(0)), 5);
        a.send("hi").await.unwrap();
        let result = last_tool_result(&llm);
        assert_eq!(result["is_error"], true);
        assert!(result["content"].as_str().unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn denied_publish_never_runs() {
        let llm = Arc::new(ScriptedLlm::new(vec![tool_call("t1", json!({})), answer("ok")]));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut a = agent(llm.clone(), Risk::Publish, calls.clone(), 5);
        a.send("publish it").await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let result = last_tool_result(&llm);
        assert_eq!(result["is_error"], true);
        assert!(result["content"].as_str().unwrap().contains("declined"));
    }

    #[tokio::test]
    async fn stops_at_turn_limit_and_conversation_stays_valid() {
        let llm = Arc::new(ScriptedLlm::new(vec![
            tool_call("t1", json!({})),
            tool_call("t2", json!({})),
            answer("second message answered"),
        ]));
        let mut a = agent(llm.clone(), Risk::Read, Arc::new(AtomicUsize::new(0)), 2);
        let reply = a.send("loop").await.unwrap();
        assert_eq!(reply.outcome, Outcome::TurnLimit);

        // Roles must alternate for the next request to be accepted.
        let reply = a.send("next").await.unwrap();
        assert_eq!(reply.text, "second message answered");
        let requests = llm.requests.lock().unwrap();
        let roles: Vec<&str> = requests.last().unwrap().messages.iter().map(|m| m.role.as_str()).collect();
        for pair in roles.windows(2) {
            assert_ne!(pair[0], pair[1], "roles must alternate: {roles:?}");
        }
    }

    #[tokio::test]
    async fn unknown_tool_is_reported() {
        let resp: Response = serde_json::from_value(json!({
            "content": [{ "type": "tool_use", "id": "t1", "name": "nope", "input": {} }],
            "stop_reason": "tool_use"
        }))
        .unwrap();
        let llm = Arc::new(ScriptedLlm::new(vec![resp, answer("ok")]));
        let mut a = agent(llm.clone(), Risk::Read, Arc::new(AtomicUsize::new(0)), 5);
        a.send("hi").await.unwrap();
        assert!(last_tool_result(&llm)["content"].as_str().unwrap().contains("unknown tool"));
    }

    #[test]
    fn long_results_are_truncated() {
        let big = Value::String("é".repeat(MAX_TOOL_RESULT_CHARS));
        let r = tool_result("t", &big, false);
        let text = r["content"].as_str().unwrap();
        assert!(text.len() <= MAX_TOOL_RESULT_CHARS + 20);
        assert!(text.ends_with("[truncated]"));
    }
}
