pub mod constants;
pub mod error;
pub mod events;
pub mod instructions;
pub mod state;

use anchor_lang::prelude::*;

pub use constants::*;
pub use events::*;
pub use instructions::*;
pub use state::*;

declare_id!("6a4UZn7JbB9uqLKiEa9gVP6zBWZtzH68jxLSoPmXV5ui");

#[program]
pub mod trend_oracle {
    use super::*;

    /// One-time setup: creates the authority's trend PDA. Must be called
    /// once per publisher before `publish_trend` can be used.
    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        initialize::handler(ctx)
    }

    /// Overwrites the authority's trend snapshot with a new one. Only the
    /// key recorded at `initialize` may call this (enforced via `has_one`).
    pub fn publish_trend(
        ctx: Context<PublishTrend>,
        label_hash: [u8; 32],
        label_preview: [u8; 32],
        score: u32,
        cluster_size: u32,
        run_id_hash: [u8; 16],
    ) -> Result<()> {
        publish_trend::handler(
            ctx,
            label_hash,
            label_preview,
            score,
            cluster_size,
            run_id_hash,
        )
    }
}
