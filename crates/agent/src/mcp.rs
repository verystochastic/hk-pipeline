//! A Model Context Protocol server that exposes the pipeline tools over stdio.
//!
//! Implements the subset of MCP a tools-only server needs: `initialize`,
//! `ping`, `tools/list` and `tools/call`. Messages are newline-delimited
//! JSON-RPC 2.0. Stdout carries protocol messages only; logs go to stderr.
//!
//! The MCP host (for example Claude Code) runs the model and decides whether
//! a call may go ahead, so this server has no approver of its own. Instead it
//! controls what is offered: `Publish` tools are never exposed, and
//! `read_only` hides `Write` tools as well.

use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::tool::{Risk, ToolRegistry};

/// Protocol revisions this server can speak, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

pub struct McpServer {
    tools: ToolRegistry,
    max_risk: Risk,
}

impl McpServer {
    pub fn new(tools: ToolRegistry, read_only: bool) -> Self {
        Self {
            tools,
            max_risk: if read_only { Risk::Read } else { Risk::Write },
        }
    }

    fn exposed(&self, name: &str) -> Option<std::sync::Arc<dyn crate::tool::Tool>> {
        self.tools.get(name).filter(|t| t.risk() <= self.max_risk)
    }

    /// Names of the tools this server offers.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.names().into_iter().filter(|n| self.exposed(n).is_some()).collect()
    }

    /// Reads requests until the input closes, answering each in turn.
    pub async fn serve<R, W>(&self, reader: R, mut writer: W) -> Result<()>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut lines = reader.lines();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(&line).await {
                let mut out = serde_json::to_vec(&reply)?;
                out.push(b'\n');
                writer.write_all(&out).await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }

    /// Handles one JSON-RPC message. Returns `None` for notifications, which get no reply.
    pub async fn handle_line(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, PARSE_ERROR, &format!("invalid JSON: {e}"))),
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response from the client; this server never sends requests, so ignore it.
            if msg.get("id").is_some() && (msg.get("result").is_some() || msg.get("error").is_some()) {
                return None;
            }
            return Some(error(msg.get("id").cloned().unwrap_or(Value::Null), INVALID_REQUEST, "missing method"));
        };
        let Some(id) = msg.get("id").cloned() else {
            tracing::debug!(method, "notification");
            return None;
        };
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));

        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(self.list_tools()),
            "tools/call" => self.call_tool(&params).await,
            _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => error(id, code, &message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = requested
            .filter(|v| PROTOCOL_VERSIONS.contains(v))
            .unwrap_or(PROTOCOL_VERSIONS[0]);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "hk-mcp", "version": env!("CARGO_PKG_VERSION") },
            "instructions": self.instructions(),
        })
    }

    fn instructions(&self) -> String {
        let mut text = crate::MCP_INSTRUCTIONS.to_string();
        if self.max_risk == Risk::Read {
            text.push_str(
                "\n\nThis server is read-only: ingesting, trend analysis and reports are not \
                 available here. Say so if the user asks for them.",
            );
        }
        text
    }

    fn list_tools(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .specs()
            .into_iter()
            .filter_map(|spec| {
                let tool = self.exposed(&spec.name)?;
                Some(json!({
                    "name": spec.name,
                    "description": spec.description,
                    "inputSchema": spec.input_schema,
                    "annotations": { "readOnlyHint": tool.risk() == Risk::Read },
                }))
            })
            .collect();
        json!({ "tools": tools })
    }

    async fn call_tool(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or((INVALID_PARAMS, "'name' is required".to_string()))?;
        let tool = self
            .exposed(name)
            .ok_or_else(|| (INVALID_PARAMS, format!("unknown tool: {name}")))?;
        let input = params.get("arguments").cloned().unwrap_or_else(|| json!({}));

        tracing::info!(tool = name, "tools/call");
        // Tool failures are reported as results, not protocol errors, so the model can see them.
        Ok(match tool.call(input).await {
            Ok(output) => json!({
                "content": [{ "type": "text", "text": serde_json::to_string_pretty(&output).unwrap_or_default() }],
                "isError": false,
            }),
            Err(e) => json!({
                "content": [{ "type": "text", "text": format!("{e:#}") }],
                "isError": true,
            }),
        })
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use anyhow::bail;
    use async_trait::async_trait;

    struct Fake(&'static str, Risk);

    #[async_trait]
    impl Tool for Fake {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "fake"
        }
        fn input_schema(&self) -> Value {
            json!({ "type": "object", "properties": {} })
        }
        fn risk(&self) -> Risk {
            self.1
        }
        async fn call(&self, input: Value) -> Result<Value> {
            if input.get("fail").is_some() {
                bail!("it broke");
            }
            Ok(json!({ "echo": input }))
        }
    }

    fn server(read_only: bool) -> McpServer {
        let mut tools = ToolRegistry::default();
        tools.register(Fake("look", Risk::Read));
        tools.register(Fake("save", Risk::Write));
        tools.register(Fake("send", Risk::Publish));
        McpServer::new(tools, read_only)
    }

    async fn rpc(s: &McpServer, msg: Value) -> Value {
        s.handle_line(&msg.to_string()).await.expect("expected a reply")
    }

    #[tokio::test]
    async fn initialize_negotiates_version() {
        let s = server(false);
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})).await;
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], "hk-mcp");
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"1999-01-01"}})).await;
        assert_eq!(r["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[tokio::test]
    async fn notifications_get_no_reply() {
        let s = server(false);
        assert!(s.handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).await.is_none());
    }

    #[tokio::test]
    async fn publish_tools_are_never_listed() {
        let r = rpc(&server(false), json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).await;
        let names: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["look", "save"]);
        assert_eq!(r["result"]["tools"][0]["annotations"]["readOnlyHint"], true);
        assert_eq!(r["result"]["tools"][1]["annotations"]["readOnlyHint"], false);
    }

    #[tokio::test]
    async fn read_only_hides_and_refuses_writes() {
        let s = server(true);
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}})).await;
        assert!(r["result"]["instructions"].as_str().unwrap().contains("read-only"));
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})).await;
        assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 1);
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"save"}})).await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"send"}})).await;
        assert_eq!(r["error"]["code"], INVALID_PARAMS);
    }

    #[tokio::test]
    async fn tool_results_and_failures() {
        let s = server(false);
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":"a","method":"tools/call","params":{"name":"look","arguments":{"q":1}}})).await;
        assert_eq!(r["id"], "a");
        assert_eq!(r["result"]["isError"], false);
        let body: Value = serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["echo"]["q"], 1);

        let r = rpc(&s, json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"look","arguments":{"fail":true}}})).await;
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(r["result"]["content"][0]["text"], "it broke");
    }

    #[tokio::test]
    async fn protocol_errors() {
        let s = server(false);
        assert_eq!(s.handle_line("{nope").await.unwrap()["error"]["code"], PARSE_ERROR);
        let r = rpc(&s, json!({"jsonrpc":"2.0","id":1,"method":"resources/list"})).await;
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    }

    #[tokio::test]
    async fn serves_over_a_stream() {
        let s = server(false);
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#, "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#, "\n",
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#, "\n",
        );
        let mut out = Vec::new();
        s.serve(tokio::io::BufReader::new(input.as_bytes()), &mut out).await.unwrap();
        let replies: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[1], json!({"jsonrpc":"2.0","id":2,"result":{}}));
    }
}
