//! Private current-context qualification inside the original cold read snapshot.
//!
//! Guarded publication facts, their allocation/generation and actual private
//! session evidence must agree. Unknown legacy/unpublished endpoints remain
//! unavailable; corrupt storage or a changed private source poisons the whole
//! candidate. This port never restores a raw registration or reconnect bearer.

use super::*;
use crate::runtime::PreparedNativeCustodyContext;
use mez_agent::messaging::{
    MessageNativeRecovery, MessageNativeRecoveryRecord, MessageNativeRecoveryStore,
    MessageRecoveredNativeBinding, MessageService, MessageStorageScope,
};

/// Combines the held original read snapshot with actual private current context.
pub(super) fn recover(
    store: &MmpCustodyStore,
    transaction: &rusqlite::Transaction<'_>,
    context: &PreparedNativeCustodyContext,
    service: MessageService,
    scope: &MessageStorageScope,
) -> Result<(MessageService, Vec<MessageRecoveredNativeBinding>)> {
    context.revalidate()?;
    let mut port = Port {
        store,
        transaction,
        context,
    };
    let recovered = service
        .recover_native_context(scope, &mut port)
        .map_err(|e| invalid(&format!("native construction refused: {e}")))?;
    context.revalidate()?;
    Ok(recovered)
}

struct Port<'a, 'conn> {
    store: &'a MmpCustodyStore,
    transaction: &'a rusqlite::Transaction<'conn>,
    context: &'a PreparedNativeCustodyContext,
}
impl MessageNativeRecoveryStore for Port<'_, '_> {
    fn recover(&mut self, target: &mut MessageNativeRecovery) -> mez_agent::messaging::Result<()> {
        let result = (|| -> Result<()> {
            if !std::ptr::eq(&self.store.connection, &**self.transaction) {
                return Err(invalid("foreign native read snapshot"));
            }
            let state = self.store.checked_state(self.transaction)?;
            if !state.watermark_origin_trusted {
                return Err(invalid("native allocator origin is unknown"));
            }
            for agent in self.context.agents() {
                let key = crate::storage::mmp_custody::native_continuity::record_key(agent);
                let Some(record) = self
                    .store
                    .read_native_continuity_in_snapshot(self.transaction, &key)?
                else {
                    continue;
                };
                let Some(generation) = record.publication_generation() else {
                    continue;
                };
                if generation > state.generation as u64 {
                    return Err(invalid("native publication is in the future"));
                }
                if self.store.native_registration_has_ended(
                    self.transaction,
                    &record.snapshot.native,
                    record.snapshot.registered_after_sequence,
                    Some(generation),
                )? {
                    continue;
                }
                if !self.context.matches(&record.snapshot) {
                    continue;
                }
                target
                    .restore_binding(MessageNativeRecoveryRecord {
                        snapshot: record.snapshot,
                        admission_available: true,
                        delivery_available: true,
                        observed_at_ms: 0,
                    })
                    .map_err(lower_error)?;
            }
            Ok(())
        })();
        result.map_err(lower_error)
    }
}

/// Preserves source context while crossing the lower trusted-port error boundary.
fn lower_error(error: impl std::fmt::Display) -> mez_agent::messaging::MessageError {
    mez_agent::messaging::MessageError::invalid_state(format!(
        "MMP native original/context qualification: {error}"
    ))
}
