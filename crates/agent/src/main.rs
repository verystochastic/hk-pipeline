use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use embedder::Embedder;
use hk_agent::harness::{Agent, Config, Outcome};
use hk_agent::llm::AnthropicClient;
use hk_agent::pipeline_tools::{self, OracleTools, PipelineContext};
use hk_agent::tool::{Approver, DenyApprover, Policy, TerminalApprover, ToolRegistry};
use oracle::OracleClient;
use sink::SurrealSink;

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum OracleMode {
    /// No oracle tools.
    Off,
    /// Offer oracle_dry_run only.
    DryRun,
    /// Also offer oracle_publish; every call still needs typed approval.
    Publish,
}

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

    /// Ask before every write (ingest, snapshots, reports), not just publishing
    #[arg(long)]
    confirm_writes: bool,

    /// Folder for Markdown reports written by the agent
    #[arg(long, default_value = "reports")]
    reports_dir: PathBuf,

    /// Folder for JSONL transcripts of each session
    #[arg(long, default_value = "agent-runs")]
    transcript_dir: PathBuf,

    #[arg(long, env = "SURREAL_URL", default_value = "127.0.0.1:8000")]
    surreal_url: String,
    #[arg(long, env = "SURREAL_USER", default_value = "root")]
    surreal_user: String,
    #[arg(long, env = "SURREAL_PASS", default_value = "root", hide_env_values = true)]
    surreal_pass: String,

    #[arg(long, env = "MODEL_DIR", default_value = "models/bge-small")]
    model_dir: PathBuf,

    #[arg(long, env = "HELIUS_API_KEY", hide_env_values = true)]
    helius_api_key: Option<String>,

    #[arg(long, default_value_t = 0.85)]
    trend_similarity_threshold: f32,

    #[arg(long, value_enum, default_value = "off")]
    oracle_mode: OracleMode,
    #[arg(long, env = "ORACLE_CLUSTER", default_value = "devnet")]
    oracle_cluster: String,
    #[arg(long, env = "ORACLE_PUBLISHER_KEYPAIR")]
    oracle_publisher_keypair: Option<PathBuf>,
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

    let oracle = match cli.oracle_mode {
        OracleMode::Off => None,
        _ => {
            let path = cli.oracle_publisher_keypair.as_ref().context(
                "--oracle-publisher-keypair (or ORACLE_PUBLISHER_KEYPAIR) is required unless --oracle-mode off",
            )?;
            Some(OracleClient::new(&cli.oracle_cluster, path)?)
        }
    };
    let oracle_tools = match cli.oracle_mode {
        OracleMode::Off => OracleTools::None,
        OracleMode::DryRun => OracleTools::DryRun,
        OracleMode::Publish => OracleTools::DryRunAndPublish,
    };

    let embedder = Embedder::new(&cli.model_dir)?;
    let sink = SurrealSink::new(&cli.surreal_url, &cli.surreal_user, &cli.surreal_pass)
        .await
        .with_context(|| format!("could not connect to SurrealDB at {}", cli.surreal_url))?;
    sink.setup().await?;

    let ctx = Arc::new(PipelineContext::new(
        embedder,
        sink,
        oracle,
        cli.helius_api_key.filter(|k| !k.is_empty()),
        cli.reports_dir.clone(),
        cli.trend_similarity_threshold,
    ));
    let mut tools = ToolRegistry::default();
    pipeline_tools::register(&mut tools, ctx, oracle_tools);

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
