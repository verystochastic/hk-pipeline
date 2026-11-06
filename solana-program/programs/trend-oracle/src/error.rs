use anchor_lang::prelude::*;

#[error_code]
pub enum ErrorCode {
    #[msg("Only the recorded authority may publish to this trend account")]
    Unauthorized,
    #[msg("Score must be between 0 and 10000 basis points")]
    ScoreOutOfRange,
}
