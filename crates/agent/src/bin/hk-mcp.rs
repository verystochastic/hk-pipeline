//! MCP server for hk-pipeline. Lets an MCP host such as Claude Code drive the
//! pipeline tools, so the model runs on the host's account instead of an API key.
//!
//!   claude mcp add hk -- /path/to/target/release/hk-mcp

use anyhow::Result;
use clap::Parser;
use hk_agent::mcp::McpServer;
use hk_agent::setup::PipelineArgs;

#[derive(Parser)]
#[command(name = "hk-mcp", about = "MCP server exposing the hk-pipeline tools over stdio")]
struct Cli {
    #[command(flatten)]
    pipeline: PipelineArgs,

    /// Offer only tools that read local data (no ingest, snapshots or reports)
    #[arg(long)]
    read_only: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Stdout is the protocol channel, so logs must go to stderr.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,hk_agent=info")),
        )
        .init();

    let cli = Cli::parse();
    let server = McpServer::new(cli.pipeline.build_tools().await?, cli.read_only);
    tracing::info!(tools = ?server.tool_names(), "hk-mcp ready");

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    server.serve(stdin, tokio::io::stdout()).await
}
