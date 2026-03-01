use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Headline {
    pub title: String,
    pub url: Option<String>,
    pub published: Option<String>,
}

pub async fn fetch_headlines() -> Result<Vec<Headline>> {
    let url = "https://news.ycombinator.com/rss";

    let body = reqwest::get(url).await?.bytes().await?;
    let feed = feed_rs::parser::parse(&body[..])?;

    let headlines = feed
        .entries
        .into_iter()
        .filter_map(|entry| {
            let title = entry.title?.content;
            let url = entry.links.into_iter().next().map(|l| l.href);
            let published = entry.published.map(|dt| dt.to_string());
            Some(Headline {
                title,
                url,
                published,
            })
        })
        .collect();
    Ok(headlines)
}
