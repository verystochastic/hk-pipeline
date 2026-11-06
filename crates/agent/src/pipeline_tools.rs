//! Tools that wrap the hk-pipeline crates for the agent.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use connector::{Connector, GdeltConnector};
use embedder::Embedder;
use serde_json::{json, Value};
use sink::{ArticleRecord, SurrealSink};
use types::{Article, ClusterSnapshot, TrendSignal};

use crate::tool::{Risk, Tool, ToolRegistry};

pub struct PipelineContext {
    pub embedder: Arc<Embedder>,
    pub sink: SurrealSink,
    pub reports_dir: PathBuf,
    pub trend_similarity_threshold: f32,
}

impl PipelineContext {
    pub fn new(
        embedder: Embedder,
        sink: SurrealSink,
        reports_dir: PathBuf,
        trend_similarity_threshold: f32,
    ) -> Self {
        Self {
            embedder: Arc::new(embedder),
            sink,
            reports_dir,
            trend_similarity_threshold,
        }
    }

    async fn embed(&self, text: String) -> Result<Vec<f32>> {
        let embedder = self.embedder.clone();
        tokio::task::spawn_blocking(move || embedder.embed(&text)).await?
    }

    async fn ingest(&self, articles: Vec<Article>) -> Result<Value> {
        let fetched = articles.len();
        let mut stored_titles = Vec::new();
        let mut skipped = 0usize;
        for article in articles {
            let embedding = self.embed(article.title.clone()).await?;
            let inserted = self
                .sink
                .insert_if_new(ArticleRecord {
                    title: article.title.clone(),
                    url: article.url,
                    published: article.published,
                    source: article.source,
                    embedding,
                    inserted_at: chrono::Utc::now().to_rfc3339(),
                })
                .await?;
            if inserted {
                stored_titles.push(article.title);
            } else {
                skipped += 1;
            }
        }
        Ok(json!({
            "fetched": fetched,
            "stored": stored_titles.len(),
            "skipped_duplicates": skipped,
            "sample_new_titles": stored_titles.iter().take(10).collect::<Vec<_>>(),
        }))
    }

}

fn usize_arg(input: &Value, key: &str, default: usize, min: usize, max: usize) -> Result<usize> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let n = v.as_u64().with_context(|| format!("'{key}' must be a whole number"))? as usize;
            if !(min..=max).contains(&n) {
                bail!("'{key}' must be between {min} and {max}");
            }
            Ok(n)
        }
    }
}

fn str_arg<'a>(input: &'a Value, key: &str) -> Result<&'a str> {
    let s = input
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("'{key}' is required"))?
        .trim();
    if s.is_empty() {
        bail!("'{key}' must not be empty");
    }
    Ok(s)
}

/// Accepts a plain file name like `aerospace-weekly.md` and nothing that
/// could escape the reports folder.
pub fn safe_report_name(name: &str) -> Result<String> {
    let name = name.trim();
    let valid = !name.is_empty()
        && name.len() <= 80
        && !name.starts_with('.')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        bail!("filename must be a plain name using letters, digits, '-', '_' or '.'");
    }
    Ok(if name.ends_with(".md") { name.to_string() } else { format!("{name}.md") })
}

fn trend_json(rank: usize, t: &TrendSignal) -> Value {
    json!({
        "rank": rank + 1,
        "label": t.label,
        "score": t.score,
        "velocity": t.velocity,
        "current_size": t.current_size,
        "previous_size": t.previous_size,
        "sources": t.sources,
    })
}

macro_rules! tool {
    ($ty:ident, $name:literal, $risk:expr, $desc:literal, $schema:expr) => {
        struct $ty(Arc<PipelineContext>);

        impl $ty {
            fn spec_schema() -> Value {
                $schema
            }
        }

        #[async_trait]
        impl Tool for $ty {
            fn name(&self) -> &str {
                $name
            }
            fn description(&self) -> &str {
                $desc
            }
            fn input_schema(&self) -> Value {
                Self::spec_schema()
            }
            fn risk(&self) -> Risk {
                $risk
            }
            async fn call(&self, input: Value) -> Result<Value> {
                self.run(input).await
            }
        }
    };
}

tool!(
    CorpusStats,
    "corpus_stats",
    Risk::Read,
    "Summarize the stored article corpus: total articles, count per source, and the oldest and newest insert times. Use this first to see what data exists.",
    json!({ "type": "object", "properties": {} })
);

impl CorpusStats {
    async fn run(&self, _input: Value) -> Result<Value> {
        let records = self.0.sink.fetch_all_embeddings().await?;
        let mut by_source: BTreeMap<&str, usize> = BTreeMap::new();
        for r in &records {
            *by_source.entry(r.source.as_str()).or_default() += 1;
        }
        let times = records.iter().map(|r| r.inserted_at.as_str()).filter(|t| !t.is_empty());
        Ok(json!({
            "total_articles": records.len(),
            "by_source": by_source,
            "oldest_inserted_at": times.clone().min(),
            "newest_inserted_at": times.max(),
        }))
    }
}

tool!(
    SearchArticles,
    "search_articles",
    Risk::Read,
    "Semantic search over stored article titles. Returns the closest articles with cosine similarity scores (1.0 = identical).",
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "Natural-language search text" },
            "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Default 5" }
        },
        "required": ["query"]
    })
);

impl SearchArticles {
    async fn run(&self, input: Value) -> Result<Value> {
        let query = str_arg(&input, "query")?.to_string();
        let limit = usize_arg(&input, "limit", 5, 1, 20)?;
        let embedding = self.0.embed(query).await?;
        let results = self.0.sink.find_similar(&embedding, limit).await?;
        Ok(json!({ "results": results }))
    }
}

tool!(
    IngestGdelt,
    "ingest_gdelt",
    Risk::Write,
    "Fetch recent news articles matching a query from the GDELT API, embed them, and store new ones (duplicates by URL are skipped).",
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "description": "GDELT search query, e.g. 'aerospace manufacturing safety'" },
            "max_results": { "type": "integer", "minimum": 1, "maximum": 250, "description": "Default 50" }
        },
        "required": ["query"]
    })
);

impl IngestGdelt {
    async fn run(&self, input: Value) -> Result<Value> {
        let query = str_arg(&input, "query")?;
        let max = usize_arg(&input, "max_results", 50, 1, 250)?;
        let articles = GdeltConnector::new(query, max).fetch().await?;
        self.0.ingest(articles).await
    }
}

tool!(
    AnalyzeTrends,
    "analyze_trends",
    Risk::Write,
    "Cluster the whole corpus (k-means on embeddings) and rank trends by growth against the previous saved run. Saves this run's cluster snapshots so the next run can measure velocity. Returns clusters and ranked trends.",
    json!({
        "type": "object",
        "properties": {
            "clusters": { "type": "integer", "minimum": 3, "maximum": 12, "description": "Number of clusters; default scales with corpus size" },
            "save_snapshot": { "type": "boolean", "description": "Default true. Set false to explore without affecting the next run's velocity baseline." }
        }
    })
);

impl AnalyzeTrends {
    async fn run(&self, input: Value) -> Result<Value> {
        let records = self.0.sink.fetch_all_embeddings().await?;
        if records.len() < 5 {
            bail!("only {} articles stored; ingest more before analyzing", records.len());
        }
        let default_k = (records.len() / 10).clamp(3, 12);
        let k = usize_arg(&input, "clusters", default_k, 3, 12)?;
        let save = input.get("save_snapshot").and_then(Value::as_bool).unwrap_or(true);

        let summaries = tokio::task::spawn_blocking(move || drift::analyse(&records, k)).await??;

        let run_id = uuid::Uuid::new_v4().to_string();
        let captured_at = chrono::Utc::now().to_rfc3339();
        let previous = self.0.sink.fetch_previous_run_snapshots(&run_id).await?;
        let trends = drift::rank_trending(
            &summaries,
            &previous,
            &captured_at,
            self.0.trend_similarity_threshold,
        );

        if save {
            let snapshots: Vec<ClusterSnapshot> = summaries
                .iter()
                .map(|s| ClusterSnapshot {
                    run_id: run_id.clone(),
                    label: s.top_titles.first().cloned().unwrap_or_default(),
                    size: s.size,
                    centroid: s.centroid.clone(),
                    sources: s.sources.clone(),
                    captured_at: captured_at.clone(),
                })
                .collect();
            self.0.sink.save_cluster_snapshots(&snapshots).await?;
        }

        let clusters: Vec<Value> = summaries
            .iter()
            .map(|s| json!({ "size": s.size, "sources": s.sources, "top_titles": s.top_titles }))
            .collect();
        let trend_list: Vec<Value> = trends.iter().enumerate().map(|(i, t)| trend_json(i, t)).collect();

        Ok(json!({
            "run_id": run_id,
            "snapshot_saved": save,
            "had_previous_run": !previous.is_empty(),
            "clusters": clusters,
            "trends": trend_list,
        }))
    }
}

tool!(
    WriteReport,
    "write_report",
    Risk::Write,
    "Save a Markdown report to the reports folder. Overwrites a report with the same name.",
    json!({
        "type": "object",
        "properties": {
            "filename": { "type": "string", "description": "Plain file name, e.g. 'aerospace-2026-11-05.md'" },
            "markdown": { "type": "string" }
        },
        "required": ["filename", "markdown"]
    })
);

impl WriteReport {
    async fn run(&self, input: Value) -> Result<Value> {
        let name = safe_report_name(str_arg(&input, "filename")?)?;
        let markdown = str_arg(&input, "markdown")?;
        tokio::fs::create_dir_all(&self.0.reports_dir).await?;
        let path = self.0.reports_dir.join(&name);
        tokio::fs::write(&path, markdown).await?;
        Ok(json!({ "path": path.display().to_string(), "bytes": markdown.len() }))
    }
}

pub fn register(registry: &mut ToolRegistry, ctx: Arc<PipelineContext>) {
    registry.register(CorpusStats(ctx.clone()));
    registry.register(SearchArticles(ctx.clone()));
    registry.register(IngestGdelt(ctx.clone()));
    registry.register(AnalyzeTrends(ctx.clone()));
    registry.register(WriteReport(ctx));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_names_cannot_escape_the_folder() {
        assert_eq!(safe_report_name("weekly").unwrap(), "weekly.md");
        assert_eq!(safe_report_name("a-b_c.2026.md").unwrap(), "a-b_c.2026.md");
        for bad in ["../x", "/etc/passwd", "a/b.md", ".hidden", "", "a b.md", "x\\y"] {
            assert!(safe_report_name(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn numeric_args_are_bounded() {
        let input = json!({ "limit": 50, "neg": -1, "s": "x" });
        assert!(usize_arg(&input, "limit", 5, 1, 20).is_err());
        assert!(usize_arg(&input, "neg", 5, 1, 20).is_err());
        assert!(usize_arg(&input, "s", 5, 1, 20).is_err());
        assert_eq!(usize_arg(&input, "missing", 5, 1, 20).unwrap(), 5);
    }

    #[test]
    fn schemas_are_objects() {
        for schema in [
            CorpusStats::spec_schema(),
            SearchArticles::spec_schema(),
            IngestGdelt::spec_schema(),
            AnalyzeTrends::spec_schema(),
            WriteReport::spec_schema(),
        ] {
            assert_eq!(schema["type"], "object");
        }
    }
}
