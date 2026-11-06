//! Standalone connectivity smoke test for Solana devnet.
//!
//! No keys required: the public devnet RPC endpoint is free and unauthenticated
//! (just rate-limited). Run with:
//!
//!   cargo run -p connector --example devnet_check
//!
//! Optionally override the endpoint (e.g. a provider's devnet URL):
//!
//!   SOLANA_DEVNET_RPC_URL=https://... cargo run -p connector --example devnet_check

use anyhow::{Context, Result};
use serde_json::{json, Value};

const DEFAULT_DEVNET_RPC: &str = "https://api.devnet.solana.com";

async fn rpc_call(client: &reqwest::Client, url: &str, method: &str, params: Value) -> Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": "devnet-check",
        "method": method,
        "params": params,
    });

    let response: Value = client
        .post(url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("request failed for method {method}"))?
        .json()
        .await
        .with_context(|| format!("failed to parse response for method {method}"))?;

    if let Some(error) = response.get("error") {
        anyhow::bail!("RPC error for {method}: {error}");
    }

    response
        .get("result")
        .cloned()
        .with_context(|| format!("missing result for method {method}"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("SOLANA_DEVNET_RPC_URL").unwrap_or_else(|_| DEFAULT_DEVNET_RPC.to_string());
    let client = reqwest::Client::new();

    println!("Connecting to {url}");

    let slot = rpc_call(&client, &url, "getSlot", json!([{"commitment": "finalized"}])).await?;
    let slot = slot.as_u64().context("getSlot did not return a number")?;
    println!("Current finalized slot: {slot}");

    let block_height = rpc_call(&client, &url, "getBlockHeight", json!([{"commitment": "finalized"}])).await?;
    println!("Block height: {block_height}");

    // Recent slots can be skipped (no block produced), so walk backwards
    // until getBlock succeeds.
    let mut probe_slot = slot;
    let block = loop {
        let params = json!([
            probe_slot,
            {
                "encoding": "json",
                "transactionDetails": "signatures",
                "rewards": false,
                "maxSupportedTransactionVersion": 0,
            }
        ]);

        match rpc_call(&client, &url, "getBlock", params).await {
            Ok(block) => break block,
            Err(e) => {
                println!("  slot {probe_slot} unavailable ({e}), trying previous slot");
                probe_slot -= 1;
            }
        }
    };

    let signatures = block
        .get("signatures")
        .and_then(Value::as_array)
        .map(|s| s.len())
        .unwrap_or(0);
    let block_time = block.get("blockTime").cloned().unwrap_or(Value::Null);

    println!("Fetched block at slot {probe_slot}: {signatures} transactions, blockTime={block_time}");

    Ok(())
}
