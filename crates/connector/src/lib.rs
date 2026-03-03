use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article {
    pub title: String,
    pub url: Option<String>,
    pub published: Option<String>,
    pub source: String,
}

#[async_trait]
pub trait Connector: Send + Sync {
    async fn fetch(&self) -> Result<Vec<Article>>;
}

//GDELT DOC API Connector

pub struct GdeltConnector {
    pub query: String,
    pub max_results: usize,
}

impl GdeltConnector {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            max_results: 50,
        }
    }
}

#[derive(Debug, Deserialize)]
struct GdeltResponse {
    articles: Option<Vec<GdeltArticle>>,
}

#[derive(Debug, Deserialize)]
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
            query,
            self.max_results
        );

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
