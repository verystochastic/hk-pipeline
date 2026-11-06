use anchor_lang::prelude::*;

#[event]
pub struct TrendPublished {
    pub authority: Pubkey,
    pub label_hash: [u8; 32],
    pub score: u32,
    pub cluster_size: u32,
    pub published_at: i64,
}
