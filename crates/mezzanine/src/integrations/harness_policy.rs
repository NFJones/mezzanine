//! Forward admission policy for explicitly retired external harness identities.
//!
//! This is product policy, not executable detection or provider/model filtering.
//! Historical decoding, queries and already-admitted completion remain separate;
//! retirement never deletes or relabels previously incurred expense.

use crate::error::{MezError, Result};

/// Rejects the exact retired canonical harness before allocating live authority
/// or admitting fresh accounting. Other identifiers retain existing validation.
pub(crate) fn require_active_external_harness(harness: &str) -> Result<()> {
    if harness == "gemini" {
        return Err(MezError::forbidden(
            "Gemini external harness support is retired",
        ));
    }
    Ok(())
}
