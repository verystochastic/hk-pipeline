//! Standalone check for the mainnet coins/NFTs/transactions connector.
//!
//! Requires HELIUS_API_KEY (get a free key at helius.dev). Run with:
//!
//!   HELIUS_API_KEY=... cargo run -p connector --example solana_trends

use connector::{Connector, SolanaConnector};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = std::env::var("HELIUS_API_KEY")
        .map_err(|_| anyhow::anyhow!("HELIUS_API_KEY must be set"))?;

    let connector = SolanaConnector::new(api_key, 10);
    let articles = connector.fetch().await?;

    println!("Fetched {} articles total\n", articles.len());

    let mut sources: Vec<&str> = articles.iter().map(|a| a.source.as_str()).collect();
    sources.sort();
    sources.dedup();

    for source in sources {
        let matching: Vec<_> = articles.iter().filter(|a| a.source == source).collect();
        println!("-- {source} ({}) --", matching.len());
        for a in matching.iter().take(5) {
            println!("  {}", a.title);
        }
        println!();
    }

    Ok(())
}
