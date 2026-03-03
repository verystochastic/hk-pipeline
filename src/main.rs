use connector::{Connector, GdeltConnector};
use embedder::Embedder;
use sink::{ArticleRecord, SimilarArticle, SurrealSink};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize all three stages
    let connector = GdeltConnector::new("aerospace manufacturing safety");
    let embedder = Embedder::new()?;
    let sink = SurrealSink::new().await?;
    sink.setup().await?;

    // Fetch
    println!("Fetching articles...");
    let articles = connector.fetch().await?;
    println!("Fetched {} articles", articles.len());

    // Embed and store
    println!("Embedding and storing...");
    let mut stored = 0;
    let mut skipped = 0;

    for article in &articles {
        let embedding = embedder.embed(&article.title)?;
        let inserted = sink
            .insert_if_new(ArticleRecord {
                title: article.title.clone(),
                url: article.url.clone(),
                published: article.published.clone(),
                source: article.source.clone(),
                embedding,
            })
            .await?;
        if inserted {
            stored += 1;
            println!("Stored:   {}", article.title);
        } else {
            skipped += 1;
            println!("Skipped:  {}", article.title);
        }
    }

    println!("\nDone! {} stored, {} duplicates skipped.", stored, skipped);

    // Similarity search
    println!("\n--- Similarity Search ---");
    let queries = vec![
        "Boeing production problems and safety",
        "electric aircraft and sustainable aviation",
        "defense contracts and military aerospace",
    ];

    for query in queries {
        println!("\nQuery: '{}'", query);
        let query_embedding = embedder.embed(query)?;
        let similar = sink.find_similar(&query_embedding, 3).await?;

        if similar.is_empty() {
            println!("  No results found.");
        } else {
            for (i, article) in similar.iter().enumerate() {
                println!(
                    "  {}. {} (score: {:.3})",
                    i + 1,
                    article.title,
                    article.score
                );
            }
        }
    }

    Ok(())
}
