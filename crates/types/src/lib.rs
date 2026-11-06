use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article {
    pub title: String,
    pub url: Option<String>,
    pub published: Option<String>,
    pub source: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArticleRecord {
    pub title: String,
    // Omitted rather than sent as null: SurrealDB rejects NULL for `option<string>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    pub source: String,
    pub embedding: Vec<f32>,
    pub inserted_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SimilarArticle {
    pub title: String,
    pub url: Option<String>,
    pub source: String,
    pub score: f32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EmbeddingRecord {
    pub title: String,
    pub source: String,
    pub inserted_at: String,
    pub embedding: Vec<f32>,
}

/// A persisted snapshot of one drift cluster from a single pipeline run,
/// used to detect trend velocity by comparing runs over time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterSnapshot {
    pub run_id: String,
    pub label: String,
    pub size: usize,
    pub centroid: Vec<f32>,
    pub sources: Vec<String>,
    pub captured_at: String,
}

/// A trend detected by comparing a current cluster against its nearest
/// match in the previous run's snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendSignal {
    pub label: String,
    pub current_size: usize,
    pub previous_size: usize,
    pub velocity: f32,
    pub score: f32,
    pub sources: Vec<String>,
    pub as_of: String,
}
