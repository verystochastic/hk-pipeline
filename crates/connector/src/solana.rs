use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::{Article, Connector};

/// Pulls Solana mainnet activity into `Article`s the rest of the pipeline
/// (embed -> dedup -> drift) already understands, across three categories:
///
/// - Recently-created SPL tokens ("new coins"), via Helius DAS `searchAssets`.
/// - Recently-created NFTs, via the same DAS call with a different asset type.
/// - A sample of recent chain-wide transactions, drawn straight from the
///   latest finalized block (not scoped to any address) and categorized by
///   Helius's transaction parser (SWAP, NFT_SALE, TRANSFER, TOKEN_MINT, …).
///
/// All reads are unauthenticated-to-the-chain (Helius just needs an API
/// key, not a wallet) — nothing here ever signs a transaction.
///
/// Token/NFT metadata and transaction descriptions are attacker-controlled
/// free text (anyone can mint a token or NFT with any name/symbol, and
/// transaction memos are arbitrary) and must be treated as untrusted: it is
/// only ever embedded as a vector or stored as an opaque on-chain hash,
/// never interpreted or executed.
pub struct SolanaConnector {
    api_key: String,
    das_url: String,
    enhanced_api_url: String,
    limit: usize,
}

impl SolanaConnector {
    pub fn new(api_key: impl Into<String>, limit: usize) -> Self {
        Self {
            api_key: api_key.into(),
            das_url: "https://mainnet.helius-rpc.com".to_string(),
            enhanced_api_url: "https://api.helius.xyz".to_string(),
            limit,
        }
    }

    /// Override the DAS/RPC base URL (e.g. a Triton endpoint, or Helius's
    /// devnet host). Defaults to Helius mainnet, since meaningful
    /// token-launch and transaction activity is a mainnet phenomenon.
    pub fn with_das_url(mut self, das_url: impl Into<String>) -> Self {
        self.das_url = das_url.into();
        self
    }
}

#[derive(Debug, serde::Serialize)]
struct JsonRpcRequest<T> {
    jsonrpc: &'static str,
    id: &'static str,
    method: &'static str,
    params: T,
}

#[derive(Debug, serde::Serialize)]
struct SearchAssetsParams {
    #[serde(rename = "tokenType")]
    token_type: &'static str,
    #[serde(rename = "sortBy")]
    sort_by: SortBy,
    limit: usize,
}

#[derive(Debug, serde::Serialize)]
struct SortBy {
    #[serde(rename = "sortBy")]
    sort_by: &'static str,
    #[serde(rename = "sortDirection")]
    sort_direction: &'static str,
}

#[derive(Debug, serde::Deserialize)]
struct SearchAssetsResponse {
    result: Option<SearchAssetsResult>,
    error: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
struct SearchAssetsResult {
    items: Vec<DasAsset>,
}

#[derive(Debug, serde::Deserialize)]
struct DasAsset {
    id: String,
    content: Option<DasContent>,
    created_at: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct DasContent {
    metadata: Option<DasMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct DasMetadata {
    name: Option<String>,
    symbol: Option<String>,
    description: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<Value>,
}

#[derive(Debug, serde::Deserialize)]
struct HeliusTransaction {
    signature: String,
    timestamp: Option<i64>,
    description: Option<String>,
    #[serde(rename = "type")]
    tx_type: Option<String>,
}

/// Helius's bulk transaction-parse endpoint accepts at most 100 signatures
/// per request.
const MAX_TRANSACTIONS_PER_PARSE: usize = 100;

/// How many finalized slots to walk backwards through looking for a
/// produced block, before giving up (a handful of slots are skipped
/// during normal chain operation).
const MAX_SLOT_PROBE_ATTEMPTS: u64 = 20;

impl SolanaConnector {
    async fn fetch_assets(&self, token_type: &'static str, source: &'static str) -> Result<Vec<Article>> {
        let url = format!("{}/?api-key={}", self.das_url, self.api_key);
        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id: "hk-pipeline",
            method: "searchAssets",
            params: SearchAssetsParams {
                token_type,
                sort_by: SortBy {
                    sort_by: "created",
                    sort_direction: "desc",
                },
                limit: self.limit,
            },
        };

        tracing::info!(token_type, limit = self.limit, "Fetching recently-created assets from Helius DAS");
        let response = reqwest::Client::new().post(&url).json(&body).send().await?;
        let parsed: SearchAssetsResponse = response.json().await?;

        if let Some(error) = parsed.error {
            anyhow::bail!("Helius DAS searchAssets error for {token_type}: {error}");
        }

        let articles = parsed
            .result
            .map(|r| r.items)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|asset| {
                let metadata = asset.content.as_ref().and_then(|c| c.metadata.as_ref())?;
                let name = metadata.name.clone().filter(|n| !n.is_empty())?;
                let symbol = metadata.symbol.clone().unwrap_or_default();
                let description = metadata.description.clone().unwrap_or_default();
                let title = match (symbol.is_empty(), description.is_empty()) {
                    (true, true) => name,
                    (true, false) => format!("{name} — {description}"),
                    (false, true) => format!("{name} ({symbol})"),
                    (false, false) => format!("{name} ({symbol}) — {description}"),
                };
                Some(Article {
                    title,
                    url: Some(format!("https://explorer.solana.com/address/{}", asset.id)),
                    published: asset.created_at.clone(),
                    source: source.to_string(),
                })
            })
            .collect();

        Ok(articles)
    }

    async fn rpc_call(&self, client: &reqwest::Client, url: &str, method: &'static str, params: Value) -> Result<Value> {
        let body = JsonRpcRequest {
            jsonrpc: "2.0",
            id: "hk-pipeline",
            method,
            params,
        };
        let response: RpcResponse = client.post(url).json(&body).send().await?.json().await?;

        if let Some(error) = response.error {
            anyhow::bail!("RPC error for {method}: {error}");
        }
        response.result.with_context(|| format!("missing result for {method}"))
    }

    /// Grabs the signature list from the most recent finalized block that
    /// was actually produced, walking backwards a few slots if needed.
    async fn fetch_recent_signatures(&self) -> Result<Vec<String>> {
        let client = reqwest::Client::new();
        let url = format!("{}/?api-key={}", self.das_url, self.api_key);

        let slot = self
            .rpc_call(&client, &url, "getSlot", serde_json::json!([{"commitment": "finalized"}]))
            .await?;
        let mut probe_slot = slot.as_u64().context("getSlot did not return a number")?;

        for _ in 0..MAX_SLOT_PROBE_ATTEMPTS {
            let params = serde_json::json!([
                probe_slot,
                {
                    "encoding": "json",
                    "transactionDetails": "signatures",
                    "rewards": false,
                    "maxSupportedTransactionVersion": 0,
                }
            ]);

            match self.rpc_call(&client, &url, "getBlock", params).await {
                Ok(block) => {
                    let signatures = block
                        .get("signatures")
                        .and_then(Value::as_array)
                        .map(|sigs| {
                            sigs.iter()
                                .filter_map(|s| s.as_str().map(String::from))
                                .take(self.limit.min(MAX_TRANSACTIONS_PER_PARSE))
                                .collect()
                        })
                        .unwrap_or_default();
                    return Ok(signatures);
                }
                Err(e) => {
                    tracing::debug!(slot = probe_slot, error = %e, "Slot unavailable, trying previous slot");
                    probe_slot -= 1;
                }
            }
        }

        anyhow::bail!("no block found in the last {MAX_SLOT_PROBE_ATTEMPTS} finalized slots");
    }

    /// Samples the latest finalized block chain-wide and categorizes every
    /// transaction in it via Helius's parser.
    async fn fetch_categorized_transactions(&self) -> Result<Vec<Article>> {
        let signatures = self.fetch_recent_signatures().await?;
        if signatures.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{}/v0/transactions?api-key={}", self.enhanced_api_url, self.api_key);
        let body = serde_json::json!({ "transactions": signatures });

        tracing::info!(count = signatures.len(), "Parsing and categorizing recent transactions via Helius");
        let response = reqwest::Client::new().post(&url).json(&body).send().await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            anyhow::bail!("Helius bulk transaction parse error ({status}): {text}");
        }
        let transactions: Vec<HeliusTransaction> = response.json().await?;

        let articles = transactions
            .into_iter()
            .map(|tx| {
                let category = tx.tx_type.unwrap_or_else(|| "UNKNOWN".to_string());
                let title = tx
                    .description
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| format!("{category} transaction"));
                Article {
                    title,
                    url: Some(format!("https://explorer.solana.com/tx/{}", tx.signature)),
                    published: tx.timestamp.map(|t| t.to_string()),
                    source: format!("solana-tx-{}", category.to_lowercase()),
                }
            })
            .collect();

        Ok(articles)
    }
}

#[async_trait]
impl Connector for SolanaConnector {
    async fn fetch(&self) -> Result<Vec<Article>> {
        let mut articles = Vec::new();

        articles.extend(self.fetch_assets("fungible", "solana-token").await?);
        articles.extend(self.fetch_assets("nonFungible", "solana-nft").await?);

        match self.fetch_categorized_transactions().await {
            Ok(txs) => articles.extend(txs),
            Err(e) => tracing::warn!(error = %e, "Skipping transaction sampling"),
        }

        Ok(articles)
    }
}
