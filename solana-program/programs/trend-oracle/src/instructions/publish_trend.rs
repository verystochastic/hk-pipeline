use anchor_lang::prelude::*;

use crate::constants::{MAX_SCORE_BPS, SEED_TREND};
use crate::error::ErrorCode;
use crate::events::TrendPublished;
use crate::state::TrendSnapshot;

#[derive(Accounts)]
pub struct PublishTrend<'info> {
    pub authority: Signer<'info>,

    #[account(
        mut,
        seeds = [SEED_TREND, authority.key().as_ref()],
        bump = trend_snapshot.bump,
        has_one = authority @ ErrorCode::Unauthorized
    )]
    pub trend_snapshot: Account<'info, TrendSnapshot>,
}

pub fn handler(
    ctx: Context<PublishTrend>,
    label_hash: [u8; 32],
    label_preview: [u8; 32],
    score: u32,
    cluster_size: u32,
    run_id_hash: [u8; 16],
) -> Result<()> {
    require!(score <= MAX_SCORE_BPS, ErrorCode::ScoreOutOfRange);

    let trend_snapshot = &mut ctx.accounts.trend_snapshot;
    trend_snapshot.label_hash = label_hash;
    trend_snapshot.label_preview = label_preview;
    trend_snapshot.score = score;
    trend_snapshot.cluster_size = cluster_size;
    trend_snapshot.run_id_hash = run_id_hash;
    trend_snapshot.published_at = Clock::get()?.unix_timestamp;

    emit!(TrendPublished {
        authority: ctx.accounts.authority.key(),
        label_hash,
        score,
        cluster_size,
        published_at: trend_snapshot.published_at,
    });

    Ok(())
}
