use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;
use hk_agent::harness::{Agent, Config, Outcome};
use hk_agent::llm::AnthropicClient;
use hk_agent::setup::PipelineArgs;
use hk_agent::tool::{Approver, DenyApprover, Policy, TerminalApprover};

#[derive(Parser)]
#[command(name = "hk-agent", about = "Claude-driven analyst for hk-pipeline")]
struct Cli {
    /// Question or task. Omit to start an interactive session.
    prompt: Option<String>,

    #[arg(long, env = "ANTHROPIC_API_KEY", hide_env_values = true)]
    api_key: String,

    #[arg(long, env = "HK_AGENT_MODEL", default_value = "claude-opus-5-5")]
    model: String,

    /// Maximum tokens per model response
    #[arg(long, default_value_t = 8192)]
    max_tokens: u32,

    /// Maximum model calls per message
    #[arg(long, default_value_t = 25)]
    max_turns: usize,

    /// Ask before every write (ingest, snapshots, reports)
    #[arg(long)]
    confirm_writes: bool,

    /// Folder for JSONL transcripts of each session
    #[arg(long, default_value = "agent-runs")]
    transcript_dir: PathBuf,

    #[command(flatten)]
    pipeline: PipelineArgs,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,hk_agent=info")),
        )
        .init();

    let cli = Cli::parse();
    let interactive = cli.prompt.is_none();

    let tools = cli.pipeline.build_tools().await?;

    // A one-shot run with piped input has nobody to ask, so approvals are refused.
    let approver: Arc<dyn Approver> = if interactive || atty_stdin() {
        Arc::new(TerminalApprover)
    } else {
        Arc::new(DenyApprover)
    };

    let transcript = cli
        .transcript_dir
        .join(format!("{}.jsonl", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")));
    let mut agent = Agent::new(
        Arc::new(AnthropicClient::new(cli.api_key)?),
        tools.clone(),
        approver,
        Config {
            model: cli.model,
            max_tokens: cli.max_tokens,
            max_turns: cli.max_turns,
            system: hk_agent::SYSTEM_PROMPT.to_string(),
            policy: Policy { auto_approve_writes: !cli.confirm_writes },
        },
    )
    .with_transcript(&transcript)?;

    eprintln!("hk-agent · tools: {} · transcript: {}", tools.names().join(", "), transcript.display());

    if let Some(prompt) = cli.prompt {
        run_turn(&mut agent, &prompt).await?;
    } else {
        eprintln!("Type a question, or 'exit' to quit.");
        let stdin = std::io::stdin();
        loop {
            eprint!("\n> ");
            std::io::stderr().flush()?;
            let mut line = String::new();
            if stdin.lock().read_line(&mut line)? == 0 {
                break;
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if matches!(line, "exit" | "quit") {
                break;
            }
            if let Err(e) = run_turn(&mut agent, line).await {
                eprintln!("error: {e:#}");
            }
        }
    }

    let u = agent.usage();
    eprintln!(
        "tokens · input {} · output {} · cache read {} · cache write {}",
        u.input_tokens, u.output_tokens, u.cache_read_input_tokens, u.cache_creation_input_tokens
    );
    Ok(())
}

async fn run_turn(agent: &mut Agent, prompt: &str) -> Result<()> {
    let reply = agent.send(prompt).await?;
    println!("{}", reply.text);
    match reply.outcome {
        Outcome::Done => {}
        Outcome::TurnLimit => eprintln!("(stopped: reached --max-turns before finishing)"),
        Outcome::Stopped(reason) => eprintln!("(stopped: {reason})"),
    }
    Ok(())
}

fn atty_stdin() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}
