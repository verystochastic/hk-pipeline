#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let headlines = connector::fetch_headlines().await?;
    let embedder = embedder::Embedder::new()?;

    for h in headlines.iter().take(3) {
        let embedding = embedder.embed(&h.title)?;

        println!("Title: {}", h.title);
        println!("Embedding dims: {}", embedding.len());
        println!("First 5 values: {:?}\n", &embedding[..5]);
    }

    println!("\nFetched {} headlines", headlines.len());
    Ok(())
}
