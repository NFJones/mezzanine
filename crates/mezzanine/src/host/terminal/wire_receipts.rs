//! Bounded positive receipt identities for outbound snapshot presentation.
//!
//! These IDs are display evidence, not execution or input authority. Snapshot
//! delivery does not acknowledge them; consumers must explicitly commit output.
//! Empty snapshots are valid, but mutation requests require nonempty exact IDs.

use crate::error::{MezError, Result};

/// Parses the producer's bounded receipt list without accepting unknown types.
pub(crate) fn parse_receipts(value: &serde_json::Value) -> Result<Vec<u64>> {
    let values = value
        .as_array()
        .filter(|values| values.len() <= 3)
        .ok_or_else(|| MezError::invalid_state("snapshot receipt list invalid"))?;
    let ids = values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| MezError::invalid_state("snapshot receipt identity invalid"))
        })
        .collect::<Result<Vec<_>>>()?;
    validate_receipts(&ids)?;
    Ok(ids)
}

/// Enforces finite, positive, distinct identities while retaining source order.
pub(crate) fn validate_receipts(ids: &[u64]) -> Result<()> {
    if ids.len() > 3
        || ids
            .iter()
            .enumerate()
            .any(|(index, id)| *id == 0 || ids[..index].contains(id))
    {
        return Err(MezError::invalid_state(
            "snapshot receipt identities invalid",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
