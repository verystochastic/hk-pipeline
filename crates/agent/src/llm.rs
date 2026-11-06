//! Minimal client for the Anthropic Messages API with tool use.
//!
//! Message content is kept as raw JSON blocks so that block types this crate
//! doesn't model (thinking, server tools, future additions) are sent back to
//! the API unchanged on the next turn.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const API_URL: &str = "https://api.anthropic.com/v1/messages";
pub const API_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<Value>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: vec![json!({ "type": "text", "text": text.into() })],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    pub system: String,
    pub tools: Vec<ToolSpec>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub content: Vec<Value>,
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}

/// A tool call requested by the model.
#[derive(Debug, Clone)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

impl Response {
    pub fn tool_uses(&self) -> Vec<ToolUse> {
        self.content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|b| {
                Some(ToolUse {
                    id: b.get("id")?.as_str()?.to_string(),
                    name: b.get("name")?.as_str()?.to_string(),
                    input: b.get("input").cloned().unwrap_or_else(|| json!({})),
                })
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, request: &Request) -> Result<Response>;
}

pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: String,
    url: String,
    max_retries: u32,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self {
            http,
            api_key: api_key.into(),
            url: API_URL.to_string(),
            max_retries: 4,
        })
    }

    /// Builds the request body. The system prompt and tool list are marked
    /// cacheable because they are identical on every turn.
    fn body(request: &Request) -> Value {
        let mut tools = serde_json::to_value(&request.tools).unwrap_or_else(|_| json!([]));
        if let Some(last) = tools.as_array_mut().and_then(|t| t.last_mut()) {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        json!({
            "model": request.model,
            "max_tokens": request.max_tokens,
            "system": [{
                "type": "text",
                "text": request.system,
                "cache_control": { "type": "ephemeral" },
            }],
            "tools": tools,
            "messages": request.messages,
        })
    }
}

fn is_retryable(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.as_u16() == 529 || status.is_server_error()
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(&self, request: &Request) -> Result<Response> {
        let body = Self::body(request);
        let mut attempt = 0;
        loop {
            let result = self
                .http
                .post(&self.url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .json(&body)
                .send()
                .await;

            let retry_after = match result {
                Ok(resp) if resp.status().is_success() => {
                    return resp.json().await.context("failed to parse API response");
                }
                Ok(resp) if is_retryable(resp.status()) && attempt < self.max_retries => {
                    let wait = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    tracing::warn!(status = %resp.status(), attempt, "API busy, retrying");
                    wait
                }
                Ok(resp) => {
                    let status = resp.status();
                    let text = resp.text().await.unwrap_or_default();
                    bail!("Anthropic API returned {status}: {text}");
                }
                Err(e) if (e.is_timeout() || e.is_connect()) && attempt < self.max_retries => {
                    tracing::warn!(error = %e, attempt, "API request failed, retrying");
                    None
                }
                Err(e) => return Err(e).context("Anthropic API request failed"),
            };

            let backoff = retry_after.unwrap_or(2u64.pow(attempt)).min(60);
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            attempt += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_marks_system_and_last_tool_cacheable() {
        let request = Request {
            model: "m".into(),
            max_tokens: 10,
            system: "sys".into(),
            tools: vec![
                ToolSpec { name: "a".into(), description: "".into(), input_schema: json!({}) },
                ToolSpec { name: "b".into(), description: "".into(), input_schema: json!({}) },
            ],
            messages: vec![Message::user_text("hi")],
        };
        let body = AnthropicClient::body(&request);
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn parses_tool_uses_and_text() {
        let resp: Response = serde_json::from_value(json!({
            "content": [
                { "type": "text", "text": "Let me look." },
                { "type": "tool_use", "id": "t1", "name": "search", "input": { "q": "x" } },
                { "type": "thinking", "thinking": "..." }
            ],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 5, "output_tokens": 3 }
        }))
        .unwrap();
        assert_eq!(resp.text(), "Let me look.");
        let uses = resp.tool_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].name, "search");
        assert_eq!(uses[0].input["q"], "x");
    }
}
