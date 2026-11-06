use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use anchor_client::anchor_lang::prelude::Pubkey;
use anchor_client::{Client, Cluster, Program, Signer};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use solana_keypair::{read_keypair_file, Keypair};
use solana_signature::Signature;

use trend_oracle::{accounts, instruction, TrendSnapshot, ID as PROGRAM_ID, SEED_TREND};
use types::TrendSignal;

/// Off-chain publisher for the trend-oracle Anchor program. Wraps
/// `anchor-client` so the rest of the pipeline (async, `anyhow::Result`,
/// `tracing` logs) doesn't need to touch Solana types directly.
pub struct OracleClient {
    program: Program<Arc<Keypair>>,
    authority: Arc<Keypair>,
}

impl OracleClient {
    /// `cluster` accepts a named cluster ("devnet", "mainnet", "localnet")
    /// or an `http(s)://` RPC URL (the websocket URL is derived from it).
    pub fn new(cluster: &str, publisher_keypair_path: &Path) -> Result<Self> {
        let keypair = read_keypair_file(publisher_keypair_path)
            .map_err(|e| anyhow::anyhow!("failed to read publisher keypair: {e}"))?;
        let authority = Arc::new(keypair);

        let cluster = Cluster::from_str(cluster).context("invalid Solana cluster/RPC URL")?;
        let client = Client::new(cluster, authority.clone());
        let program = client
            .program(PROGRAM_ID)
            .context("failed to build anchor-client Program handle")?;

        Ok(Self { program, authority })
    }

    pub fn authority_pubkey(&self) -> Pubkey {
        self.authority.pubkey()
    }

    fn trend_pda(&self) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[SEED_TREND, self.authority.pubkey().as_ref()], &PROGRAM_ID)
    }

    /// Creates this publisher's trend PDA if it doesn't exist yet. Returns
    /// `None` if it was already initialized (no transaction sent).
    pub async fn ensure_initialized(&self) -> Result<Option<Signature>> {
        let (trend_snapshot, _bump) = self.trend_pda();
        if self
            .program
            .account::<TrendSnapshot>(trend_snapshot)
            .await
            .is_ok()
        {
            return Ok(None);
        }

        let sig = self
            .program
            .request()
            .accounts(accounts::Initialize {
                payer: self.authority.pubkey(),
                authority: self.authority.pubkey(),
                trend_snapshot,
                system_program: anchor_client::anchor_lang::system_program::ID,
            })
            .args(instruction::Initialize {})
            .send()
            .await
            .context("failed to send initialize transaction")?;

        Ok(Some(sig))
    }

    /// Builds the `publish_trend` instruction and logs it without any
    /// network I/O. This is the default mode — actually publishing requires
    /// an explicit operator opt-in (see the pipeline's `--oracle-mode` flag).
    pub fn simulate_publish(&self, signal: &TrendSignal, run_id: &str) -> Result<()> {
        let (trend_snapshot, _bump) = self.trend_pda();
        let payload = TrendPayload::from_signal(signal, run_id);

        let ixs = self
            .program
            .request()
            .accounts(accounts::PublishTrend {
                authority: self.authority.pubkey(),
                trend_snapshot,
            })
            .args(instruction::PublishTrend {
                label_hash: payload.label_hash,
                label_preview: payload.label_preview,
                score: payload.score_bps,
                cluster_size: payload.cluster_size,
                run_id_hash: payload.run_id_hash,
            })
            .instructions();

        tracing::info!(
            label = %signal.label,
            score_bps = payload.score_bps,
            cluster_size = payload.cluster_size,
            trend_snapshot = %trend_snapshot,
            authority = %self.authority.pubkey(),
            instructions = ?ixs,
            "DRY RUN: would publish trend (no transaction sent)"
        );
        Ok(())
    }

    pub async fn publish_trend(&self, signal: &TrendSignal, run_id: &str) -> Result<Signature> {
        let (trend_snapshot, _bump) = self.trend_pda();
        let payload = TrendPayload::from_signal(signal, run_id);

        let sig = self
            .program
            .request()
            .accounts(accounts::PublishTrend {
                authority: self.authority.pubkey(),
                trend_snapshot,
            })
            .args(instruction::PublishTrend {
                label_hash: payload.label_hash,
                label_preview: payload.label_preview,
                score: payload.score_bps,
                cluster_size: payload.cluster_size,
                run_id_hash: payload.run_id_hash,
            })
            .send()
            .await
            .context("failed to send publish_trend transaction")?;

        tracing::info!(
            signature = %sig,
            label = %signal.label,
            score_bps = payload.score_bps,
            "Published trend on-chain"
        );
        Ok(sig)
    }
}

struct TrendPayload {
    label_hash: [u8; 32],
    label_preview: [u8; 32],
    score_bps: u32,
    cluster_size: u32,
    run_id_hash: [u8; 16],
}

impl TrendPayload {
    fn from_signal(signal: &TrendSignal, run_id: &str) -> Self {
        Self {
            label_hash: Sha256::digest(signal.label.as_bytes()).into(),
            label_preview: label_preview_bytes(&signal.label),
            score_bps: (signal.score.clamp(0.0, 1.0) * 10_000.0) as u32,
            cluster_size: signal.current_size as u32,
            run_id_hash: Sha256::digest(run_id.as_bytes())[..16].try_into().unwrap(),
        }
    }
}

fn label_preview_bytes(label: &str) -> [u8; 32] {
    let mut buf = [0u8; 32];
    let bytes = label.as_bytes();
    let mut end = bytes.len().min(32);
    while end > 0 && !label.is_char_boundary(end) {
        end -= 1;
    }
    buf[..end].copy_from_slice(&bytes[..end]);
    buf
}
