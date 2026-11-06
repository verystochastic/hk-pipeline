use anyhow::Result;
use surrealdb::engine::remote::ws::{Client, Ws};
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;

pub use types::{ArticleRecord, ClusterSnapshot, EmbeddingRecord, SimilarArticle};

pub struct SurrealSink {
    db: Surreal<Client>,
}

impl SurrealSink {
    pub async fn new(url: &str, user: &str, password: &str) -> Result<Self> {
        tracing::debug!(%url, "Connecting to SurrealDB");
        let db = Surreal::new::<Ws>(url).await?;
        db.signin(Root {
            username: user.to_string(),
            password: password.to_string(),
        })
        .await?;
        db.use_ns("pipeline").use_db("news").await?;
        Ok(Self { db })
    }

    pub async fn setup(&self) -> Result<()> {
        self.db
            .query(
                "DEFINE TABLE OVERWRITE articles SCHEMAFULL;
                 DEFINE FIELD OVERWRITE title      ON articles TYPE string;
                 DEFINE FIELD OVERWRITE url        ON articles TYPE option<string>;
                 DEFINE FIELD OVERWRITE published  ON articles TYPE option<string>;
                 DEFINE FIELD OVERWRITE source     ON articles TYPE string;
                 DEFINE FIELD OVERWRITE embedding  ON articles TYPE array<float>;
                 DEFINE FIELD OVERWRITE inserted_at ON articles TYPE string;
                 DEFINE INDEX OVERWRITE url_unique ON articles FIELDS url UNIQUE;
                 DEFINE INDEX OVERWRITE embedding_hnsw ON articles FIELDS embedding
                   HNSW DIMENSION 384 DIST COSINE;
                 DEFINE TABLE OVERWRITE cluster_snapshots SCHEMAFULL;
                 DEFINE FIELD OVERWRITE run_id      ON cluster_snapshots TYPE string;
                 DEFINE FIELD OVERWRITE label       ON cluster_snapshots TYPE string;
                 DEFINE FIELD OVERWRITE size        ON cluster_snapshots TYPE int;
                 DEFINE FIELD OVERWRITE centroid    ON cluster_snapshots TYPE array<float>;
                 DEFINE FIELD OVERWRITE sources     ON cluster_snapshots TYPE array<string>;
                 DEFINE FIELD OVERWRITE captured_at ON cluster_snapshots TYPE string;
                 DEFINE INDEX OVERWRITE run_id_idx ON cluster_snapshots FIELDS run_id;",
            )
            .await?;
        tracing::info!("Schema and indexes applied");
        Ok(())
    }

    async fn insert(&self, record: ArticleRecord) -> Result<()> {
        self.db
            .create(("articles", uuid::Uuid::new_v4().to_string()))
            .content(serde_json::to_value(&record)?)
            .await
            .map(|_: Option<serde_json::Value>| ())
            .or_else(|e| {
                // SurrealDB SDK deserialises the returned record ID as an opaque
                // value; swallow that parse error since the insert succeeded.
                if e.to_string().contains("Expected any, got record") {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(e))
                }
            })?;
        Ok(())
    }

    pub async fn insert_if_new(&self, record: ArticleRecord) -> Result<bool> {
        if let Some(ref url) = record.url {
            let mut result = self
                .db
                .query("SELECT id FROM articles WHERE url = $url LIMIT 1")
                .bind(("url", url.clone()))
                .await?;
            let existing: Vec<serde_json::Value> = result.take(0)?;
            if !existing.is_empty() {
                tracing::debug!(%url, "Skipping duplicate");
                return Ok(false);
            }
        }
        self.insert(record).await?;
        Ok(true)
    }

    pub async fn find_similar(&self, embedding: &[f32], limit: usize) -> Result<Vec<SimilarArticle>> {
        // <|limit, ef|> triggers the HNSW index; ef=40 is a reasonable recall/speed tradeoff.
        let query = format!(
            "SELECT title, url, source, vector::similarity::cosine(embedding, $vec) AS score
             FROM articles
             WHERE embedding <|{},40|> $vec
             ORDER BY score DESC",
            limit
        );

        let mut result = self
            .db
            .query(query)
            .bind(("vec", embedding.to_vec()))
            .await?;
        let raw: Vec<serde_json::Value> = result.take(0)?;

        let articles = raw
            .into_iter()
            .filter_map(|v| {
                Some(SimilarArticle {
                    title: v.get("title")?.as_str()?.to_string(),
                    url: v.get("url").and_then(|u| u.as_str()).map(|s| s.to_string()),
                    source: v.get("source")?.as_str()?.to_string(),
                    score: v.get("score")?.as_f64()? as f32,
                })
            })
            .collect();

        Ok(articles)
    }

    pub async fn fetch_all_embeddings(&self) -> Result<Vec<EmbeddingRecord>> {
        let mut result = self
            .db
            .query("SELECT title, source, inserted_at, embedding FROM articles")
            .await?;
        let raw: Vec<serde_json::Value> = result.take(0)?;

        let records = raw
            .into_iter()
            .filter_map(|v| {
                let embedding: Vec<f32> = v
                    .get("embedding")?
                    .as_array()?
                    .iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect();
                Some(EmbeddingRecord {
                    title: v.get("title")?.as_str()?.to_string(),
                    source: v.get("source")?.as_str()?.to_string(),
                    inserted_at: v
                        .get("inserted_at")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    embedding,
                })
            })
            .collect();

        Ok(records)
    }

    pub async fn save_cluster_snapshots(&self, snapshots: &[ClusterSnapshot]) -> Result<()> {
        for snapshot in snapshots {
            self.db
                .create::<Option<serde_json::Value>>((
                    "cluster_snapshots",
                    uuid::Uuid::new_v4().to_string(),
                ))
                .content(serde_json::to_value(snapshot)?)
                .await
                .map(|_| ())
                .or_else(|e| {
                    if e.to_string().contains("Expected any, got record") {
                        Ok(())
                    } else {
                        Err(anyhow::anyhow!(e))
                    }
                })?;
        }
        Ok(())
    }

    /// Finds the most recent run before `current_run_id` and returns all of
    /// its cluster snapshots, for time-aware trend comparison. Two simple
    /// queries rather than one nested SurrealQL subquery, matching the SDK
    /// deserialization workarounds already used elsewhere in this file.
    pub async fn fetch_previous_run_snapshots(
        &self,
        current_run_id: &str,
    ) -> Result<Vec<ClusterSnapshot>> {
        let mut latest = self
            .db
            .query(
                "SELECT run_id, captured_at FROM cluster_snapshots
                 WHERE run_id != $run_id
                 ORDER BY captured_at DESC LIMIT 1",
            )
            .bind(("run_id", current_run_id.to_string()))
            .await?;
        let latest_run: Vec<serde_json::Value> = latest.take(0)?;
        let Some(prev_run_id) = latest_run.first().and_then(|v| v.get("run_id")).and_then(|v| v.as_str())
        else {
            return Ok(vec![]);
        };

        let mut result = self
            .db
            .query("SELECT run_id, label, size, centroid, sources, captured_at FROM cluster_snapshots WHERE run_id = $run_id")
            .bind(("run_id", prev_run_id.to_string()))
            .await?;
        let raw: Vec<serde_json::Value> = result.take(0)?;

        let snapshots = raw
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();

        Ok(snapshots)
    }
}
