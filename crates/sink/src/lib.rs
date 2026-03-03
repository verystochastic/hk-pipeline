use serde_json;
use uuid;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use surrealdb::engine::remote::ws::{Client, Ws};
use surrealdb::opt::auth::Root;
use surrealdb::Surreal;

#[derive(Debug, Serialize, Deserialize)]
pub struct ArticleRecord {
    pub title: String,
    pub url: Option<String>,
    pub published: Option<String>,
    pub source: String,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SimilarArticle {
    pub title: String,
    pub url: Option<String>,
    pub source: String,
    pub score: f32,
}

pub struct SurrealSink {
    db: Surreal<Client>,
}

impl SurrealSink {
    pub async fn new() -> Result<Self> {
        let db = Surreal::new::<Ws>("127.0.0.1:8000").await?;
        db.signin(Root {
            username: "root".to_string(),
            password: "root".to_string(),
        })
        .await?;
        db.use_ns("pipeline").use_db("news").await?;
        Ok(Self { db })
    }

    pub async fn setup(&self) -> Result<()> {
        self.db
            .query("DEFINE TABLE OVERWRITE articles SCHEMAFULL;
                    DEFINE FIELD OVERWRITE title ON articles TYPE string;
                    DEFINE FIELD OVERWRITE url ON articles TYPE option<string>;
                    DEFINE FIELD OVERWRITE published ON articles TYPE option<string>;
                    DEFINE FIELD OVERWRITE source ON articles TYPE string;
                    DEFINE FIELD OVERWRITE embedding ON articles TYPE array<float>;
                    DEFINE INDEX OVERWRITE url_unique ON articles FIELDS url UNIQUE;")
            .await?;
        Ok(())
    }

    pub async fn exists(&self, url: &str) -> Result<bool> {
       let url = url.to_string();
        let mut result = self.db
            .query("SELECT id FROM articles WHERE url = $url LIMIT 1")
            .bind(("url", url))
            .await?;
    
        let records: Vec<serde_json::Value> = result.take(0).or_else(|e| {
            if e.to_string().contains("Expected any, got record") {
                Ok(vec![serde_json::Value::Null])
            } else {
                Err(e)
            }
        })?;
    
        Ok(!records.is_empty() && records[0] != serde_json::Value::Null)
    }


    pub async fn insert(&self, record: ArticleRecord) -> Result<()> {
        self.db
            .create(("articles", uuid::Uuid::new_v4().to_string()))
            .content(serde_json::to_value(&record)?)
            .await
            .map(|_: Option<serde_json::Value>| ())
            .or_else(|e| {
                if e.to_string().contains("Expected any, got record") {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!(e))
                }
            })?;
        Ok(())
    }

    pub async fn insert_if_new(&self, record: ArticleRecord) -> Result<bool> {
       if record.url.is_none() {
            self.insert(record).await?;
            return Ok(true);
        }

        let result = self.db
            .create(("articles", uuid::Uuid::new_v4().to_string()))
            .content(serde_json::to_value(&record)?)
            .await
            .map(|_: Option<serde_json::Value>| true)
            .or_else(|e| {
                let msg = e.to_string();
                if msg.contains("already contains") ||
                    msg.contains("already exists") ||
                    msg.contains("unique") ||
                    msg.contains("Expected any, got record") {
                    Ok(false)
                } else {
                    Err(anyhow::anyhow!(e))
                }
            })?;

        Ok(result)
    }

}
