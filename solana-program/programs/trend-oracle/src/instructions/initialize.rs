use anchor_lang::prelude::*;

use crate::constants::SEED_TREND;
use crate::state::TrendSnapshot;

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,

    /// The publisher key that will be authorized to call `publish_trend`
    /// against this account. Must sign to prove key custody.
    pub authority: Signer<'info>,

    #[account(
        init,
        payer = payer,
        space = 8 + TrendSnapshot::INIT_SPACE,
        seeds = [SEED_TREND, authority.key().as_ref()],
        bump
    )]
    pub trend_snapshot: Account<'info, TrendSnapshot>,

    pub system_program: Program<'info, System>,
}

pub fn handler(ctx: Context<Initialize>) -> Result<()> {
    let trend_snapshot = &mut ctx.accounts.trend_snapshot;
    trend_snapshot.authority = ctx.accounts.authority.key();
    trend_snapshot.label_hash = [0u8; 32];
    trend_snapshot.label_preview = [0u8; 32];
    trend_snapshot.score = 0;
    trend_snapshot.cluster_size = 0;
    trend_snapshot.run_id_hash = [0u8; 16];
    trend_snapshot.published_at = 0;
    trend_snapshot.bump = ctx.bumps.trend_snapshot;
    Ok(())
}
