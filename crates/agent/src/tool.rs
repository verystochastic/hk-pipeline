//! Tool definitions and the approval policy that gates them.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::llm::ToolSpec;

/// How much a tool can affect the world. The harness uses this to decide
/// whether a call needs the operator's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    /// Reads local data only.
    Read,
    /// Writes to the local database or files, or calls a free public API.
    Write,
    /// Signs or sends something irreversible, such as an on-chain transaction.
    Publish,
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    fn risk(&self) -> Risk;
    async fn call(&self, input: Value) -> Result<Value>;
}

#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name().to_string(), Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.input_schema(),
            })
            .collect()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
}

/// Asks whoever is operating the agent whether a tool call may run.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, tool: &str, risk: Risk, input: &Value) -> Decision;
}

/// Which risk levels run without asking. `Publish` is never auto-approved.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub auto_approve_writes: bool,
}

impl Policy {
    pub fn needs_approval(&self, risk: Risk) -> bool {
        match risk {
            Risk::Read => false,
            Risk::Write => !self.auto_approve_writes,
            Risk::Publish => true,
        }
    }
}

/// Prompts on the terminal. Publishing requires typing the word `publish`,
/// so a stray Enter or `y` can't send a transaction.
pub struct TerminalApprover;

#[async_trait]
impl Approver for TerminalApprover {
    async fn approve(&self, tool: &str, risk: Risk, input: &Value) -> Decision {
        let (prompt, expected) = match risk {
            Risk::Publish => ("Type 'publish' to send it, anything else to refuse: ", "publish"),
            _ => ("Allow? [y/N] ", "y"),
        };
        let header = format!(
            "\n── approval needed: {tool} ({risk:?}) ──\n{}\n{prompt}",
            serde_json::to_string_pretty(input).unwrap_or_default()
        );
        let answer = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            eprint!("{header}");
            let _ = std::io::stderr().flush();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).map(|_| line)
        })
        .await;
        match answer {
            Ok(Ok(line)) if line.trim().eq_ignore_ascii_case(expected) => Decision::Allow,
            _ => Decision::Deny,
        }
    }
}

/// Refuses everything that needs approval; for non-interactive runs.
pub struct DenyApprover;

#[async_trait]
impl Approver for DenyApprover {
    async fn approve(&self, _tool: &str, _risk: Risk, _input: &Value) -> Decision {
        Decision::Deny
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_always_needs_approval() {
        let lax = Policy { auto_approve_writes: true };
        let strict = Policy { auto_approve_writes: false };
        assert!(!lax.needs_approval(Risk::Read));
        assert!(!lax.needs_approval(Risk::Write));
        assert!(lax.needs_approval(Risk::Publish));
        assert!(strict.needs_approval(Risk::Write));
        assert!(strict.needs_approval(Risk::Publish));
    }
}
