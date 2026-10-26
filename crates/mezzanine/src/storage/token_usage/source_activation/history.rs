//! Durable uncertainty before a warm observer can become established authority.
//! Tokens are inert operation identities, not credentials/native attestation.

use super::*;

/// Fixed-size decoding even for damaged or constraint-bypassed metadata.
pub(super) fn read_gap(connection: &Connection, owner: &str) -> Result<Option<[u8; 32]>> {
    let (bytes,kind):(Option<Vec<u8>>,String)=connection.query_row(
        "SELECT substr(observer_history_gap,1,33),typeof(observer_history_gap) FROM external_usage_source_activations WHERE owner=?1",
        [owner],|row|Ok((row.get(0)?,row.get(1)?)))?;
    match (bytes, kind.as_str()) {
        (None, "null") => Ok(None),
        (Some(bytes), "blob") => Ok(Some(
            bytes.try_into().map_err(|_| invalid_stored_activation())?,
        )),
        _ => Err(invalid_stored_activation()),
    }
}

impl TokenUsageStore {
    /// Strict readonly cold eligibility. Missing/old/corrupt stores are not
    /// initialized here; no accepted history may silently conceal a gap.
    pub(crate) fn external_source_history_complete(&self, key: &ExternalSourceKey) -> Result<bool> {
        let connection =
            crate::storage::shared_sqlite::open_shared_database_read_only(self.path())?
                .ok_or_else(|| {
                    MezError::invalid_state("external source history store unavailable")
                })?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version != super::super::store::SCHEMA_VERSION {
            return Err(MezError::invalid_state(
                "external source history schema unavailable",
            ));
        }
        let owner = mapped_owner(&connection, key)?;
        let gap = read_gap(&connection, &owner)?;
        Ok(gap.is_none())
    }

    /// Before NEW warm admission, mark history uncertain under the exact source
    /// and public predecessor. Failure prevents that new warm transition; old
    /// current authority is not retired. Prepared initial metadata keeps the
    /// marker through later acceptance, closing the initial-publication race.
    pub(crate) fn mark_external_source_history_gap(
        &self,
        key: &ExternalSourceKey,
        activation: &ExternalSourceActivation,
        expected: &ExternalSourceObserverFence,
        nonce: [u8; 32],
    ) -> Result<()> {
        if nonce == [0; 32] {
            return Err(invalid_stored_activation());
        }
        let mut connection = self.open()?;
        let tx = connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let owner = mapped_owner(&tx, key)?;
        let receipt = load_receipt(&tx, &owner)?.ok_or_else(invalid_stored_activation)?;
        if receipt.activation != *activation
            || receipt.state == ExternalSourceActivationState::Retired
            || (receipt.observer.as_ref() != Some(expected)
                && receipt.state != ExternalSourceActivationState::Prepared)
        {
            return Err(MezError::conflict(
                "external source history predecessor changed",
            ));
        }
        tx.execute(
            "UPDATE external_usage_source_activations SET observer_history_gap=?1 WHERE owner=?2",
            params![nonce.as_slice(), owner],
        )?;
        tx.commit()?;
        Ok(())
    }
}
