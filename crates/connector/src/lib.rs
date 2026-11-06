use anyhow::Result;
use async_trait::async_trait;
pub use types::Article;

#[async_trait]
pub trait Connector: Send + Sync {
    async fn fetch(&self) -> Result<Vec<Article>>;
}

pub struct GdeltConnector {
    query: String,
    max_results: usize,
}

impl GdeltConnector {
    pub fn new(query: impl Into<String>, max_results: usize) -> Self {
        Self {
            query: query.into(),
            max_results,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct GdeltResponse {
    articles: Option<Vec<GdeltArticle>>,
}

#[derive(Debug, serde::Deserialize)]
struct GdeltArticle {
    title: Option<String>,
    url: Option<String>,
    seendate: Option<String>,
}

#[async_trait]
impl Connector for GdeltConnector {
    async fn fetch(&self) -> Result<Vec<Article>> {
        let query = urlencoding::encode(&self.query);
        let url = format!(
            "https://api.gdeltproject.org/api/v2/doc/doc?query={}&mode=artlist&maxrecords={}&format=json",
            query, self.max_results
        );

        tracing::info!(query = %self.query, max_results = self.max_results, "Fetching from GDELT");
        let response = reqwest::get(&url).await?;
        let gdelt: GdeltResponse = response.json().await?;

        let articles = gdelt
            .articles
            .unwrap_or_default()
            .into_iter()
            .filter_map(|a| {
                let title = a.title?;
                Some(Article {
                    title,
                    url: a.url,
                    published: a.seendate,
                    source: "gdelt".to_string(),
                })
            })
            .collect();

        Ok(articles)
    }
}
