//! Connects the pipeline services and builds the tool registry. Shared by
//! `hk-agent` and `hk-mcp` so both expose exactly the same tools.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use embedder::Embedder;
use sink::SurrealSink;

use crate::pipeline_tools::{self, PipelineContext};
use crate::tool::ToolRegistry;

#[derive(clap::Args, Debug, Clone)]
pub struct PipelineArgs {
    /// Folder for Markdown reports written by the agent
    #[arg(long, default_value = "reports")]
    pub reports_dir: PathBuf,

    #[arg(long, env = "SURREAL_URL", default_value = "127.0.0.1:8000")]
    pub surreal_url: String,
    #[arg(long, env = "SURREAL_USER", default_value = "root")]
    pub surreal_user: String,
    #[arg(long, env = "SURREAL_PASS", default_value = "root", hide_env_values = true)]
    pub surreal_pass: String,

    #[arg(long, env = "MODEL_DIR", default_value = "models/bge-small")]
    pub model_dir: PathBuf,

    #[arg(long, default_value_t = 0.85)]
    pub trend_similarity_threshold: f32,
}

impl PipelineArgs {
    /// Loads the embedding model, connects to SurrealDB and registers every pipeline tool.
    pub async fn build_tools(&self) -> Result<ToolRegistry> {
        let embedder = Embedder::new(&self.model_dir)
            .with_context(|| format!("could not load the embedding model from {}", self.model_dir.display()))?;
        let sink = SurrealSink::new(&self.surreal_url, &self.surreal_user, &self.surreal_pass)
            .await
            .with_context(|| format!("could not connect to SurrealDB at {}", self.surreal_url))?;
        sink.setup().await?;

        let ctx = Arc::new(PipelineContext::new(
            embedder,
            sink,
            self.reports_dir.clone(),
            self.trend_similarity_threshold,
        ));
        let mut tools = ToolRegistry::default();
        pipeline_tools::register(&mut tools, ctx);
        Ok(tools)
    }
}
