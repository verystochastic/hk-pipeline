use anchor_lang::prelude::*;

/// Latest trend snapshot published by an authorized off-chain publisher.
/// Bounded, single-slot account — history is available via `TrendPublished`
/// events, not stored on-chain, so this account never grows.
#[account]
#[derive(InitSpace)]
pub struct TrendSnapshot {
    /// Publisher key recorded at `initialize`; only this key may publish updates.
    pub authority: Pubkey,
    /// sha256 of the full off-chain cluster label (untrusted text, never stored raw).
    pub label_hash: [u8; 32],
    /// Truncated UTF-8-lossy preview of the label, for cheap on-chain display.
    /// Untrusted — consumers must not treat this as verified text.
    pub label_preview: [u8; 32],
    /// Trend score in basis points, 0..=10_000.
    pub score: u32,
    /// Raw member count of the trending cluster, informational only.
    pub cluster_size: u32,
    /// Truncated hash of the off-chain pipeline run id that produced this snapshot.
    pub run_id_hash: [u8; 16],
    /// Set on-chain via `Clock::get()` — never client-supplied, so it can't be backdated.
    pub published_at: i64,
    pub bump: u8,
}
