use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use connector::{Connector, GdeltConnector};
use drift::{analyse, rank_trending};
use embedder::Embedder;
use sink::{ArticleRecord, SurrealSink};
use types::ClusterSnapshot;

#[derive(Parser)]
#[command(name = "hk-pipeline", about = "News article ingestion and analysis pipeline")]
struct Cli {
    /// Search query for GDELT
    #[arg(short, long, default_value = "aerospace manufacturing safety")]
    query: String,

    /// Maximum articles to fetch from GDELT
    #[arg(long, default_value_t = 50)]
    max_results: usize,

    /// SurrealDB WebSocket URL
    #[arg(long, env = "SURREAL_URL", default_value = "127.0.0.1:8000")]
    surreal_url: String,

    /// SurrealDB username
    #[arg(long, env = "SURREAL_USER", default_value = "root")]
    surreal_user: String,

    /// SurrealDB password
    #[arg(long, env = "SURREAL_PASS", default_value = "root")]
    surreal_pass: String,

    /// Path to the BGE-small model directory
    #[arg(long, env = "MODEL_DIR", default_value = "models/bge-small")]
    model_dir: PathBuf,

    /// Cosine similarity above which a current cluster is considered the
    /// same trend as a cluster from the previous run
    #[arg(long, default_value_t = 0.85)]
    trend_similarity_threshold: f32,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let connector: Box<dyn Connector> = Box::new(GdeltConnector::new(&cli.query, cli.max_results));
    let embedder = Embedder::new(&cli.model_dir)?;
    let sink = SurrealSink::new(&cli.surreal_url, &cli.surreal_user, &cli.surreal_pass).await?;
    sink.setup().await?;

    let articles = connector.fetch().await?;
    tracing::info!(count = articles.len(), "Fetched articles");

    let mut stored = 0usize;
    let mut skipped = 0usize;

    for article in &articles {
        let embedding = embedder.embed(&article.title)?;
        let inserted = sink
            .insert_if_new(ArticleRecord {
                title: article.title.clone(),
                url: article.url.clone(),
                published: article.published.clone(),
                source: article.source.clone(),
                embedding,
                inserted_at: chrono::Utc::now().to_rfc3339(),
            })
            .await?;
        if inserted {
            stored += 1;
            tracing::info!(title = %article.title, "Stored");
        } else {
            skipped += 1;
            tracing::debug!(title = %article.title, "Skipped duplicate");
        }
    }

    tracing::info!(stored, skipped, "Ingestion complete");

    let records = sink.fetch_all_embeddings().await?;
    tracing::info!(corpus = records.len(), "Running drift analysis");

    if records.len() >= 5 {
        let k = (records.len() / 10).clamp(3, 12);
        match analyse(&records, k) {
            Ok(summaries) => {
                for s in &summaries {
                    tracing::info!(
                        cluster = s.cluster_id,
                        size = s.size,
                        sources = %s.sources.join(", "),
                        "Cluster"
                    );
                    for title in &s.top_titles {
                        tracing::info!("  · {}", title);
                    }
                }

                let run_id = uuid::Uuid::new_v4().to_string();
                let captured_at = chrono::Utc::now().to_rfc3339();

                let previous = sink.fetch_previous_run_snapshots(&run_id).await?;
                let trending = rank_trending(
                    &summaries,
                    &previous,
                    &captured_at,
                    cli.trend_similarity_threshold,
                );
                for t in &trending {
                    tracing::info!(
                        label = %t.label,
                        score = t.score,
                        velocity = t.velocity,
                        current_size = t.current_size,
                        previous_size = t.previous_size,
                        "Trend"
                    );
                }

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
                sink.save_cluster_snapshots(&snapshots).await?;
            }
            Err(e) => tracing::warn!("Drift analysis skipped: {}", e),
        }
    } else {
        tracing::warn!("Too few articles for clustering — ingest more data first");
    }

    let queries = [
        "Boeing production problems and safety",
        "electric aircraft and sustainable aviation",
        "defense contracts and military aerospace",
    ];

    for query in queries {
        tracing::info!(%query, "Similarity search");
        let query_embedding = embedder.embed(query)?;
        let similar = sink.find_similar(&query_embedding, 3).await?;

        if similar.is_empty() {
            tracing::info!("  No results found");
        } else {
            for (i, article) in similar.iter().enumerate() {
                tracing::info!(
                    rank = i + 1,
                    score = article.score,
                    title = %article.title,
                    "  Result"
                );
            }
        }
    }

    Ok(())
}
