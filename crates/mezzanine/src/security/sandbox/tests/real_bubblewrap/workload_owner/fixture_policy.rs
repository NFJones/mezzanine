//! Pure deletion policy for generated synthetic kernel-fixture evidence.
//!
//! Reserve before every spawn attempt. An attempted owner is unsettled by
//! default, including spawn failure and unwind. Only explicit successful
//! collection, exact root reap, EOF/input closure and owner destruction may
//! settle that slot. Dropping an owner or reaching a deadline is not evidence.
//! This policy performs no filesystem/process/logging operations.

/// Explicit evidence assembled before and after destroying one retired owner.
#[derive(Clone, Copy, Default)]
pub(super) struct Settlement {
    /// Collection returned a status without any primary or secondary error.
    pub(super) collection_ok: bool,
    /// The sole owner actually consumed its root's wait receipt.
    pub(super) root_reaped: bool,
    /// Both bounded capture streams reached EOF.
    pub(super) capture_eof: bool,
    /// All staged input was delivered and its pipe closed.
    pub(super) input_closed: bool,
    /// Explicit retirement completed before result construction.
    pub(super) retired: bool,
    /// The owner was destroyed, closing all remaining capture descriptors.
    pub(super) handles_closed: bool,
}

impl Settlement {
    /// Requires every independent settlement fact; unknown is never true.
    fn confirmed(self) -> bool {
        self.collection_ok
            && self.root_reaped
            && self.capture_eof
            && self.input_closed
            && self.retired
            && self.handles_closed
    }
}

/// Distinct local owner slots prevent one settled root from covering another.
#[derive(Default)]
pub(super) struct FixturePolicy {
    slots: Vec<bool>,
    invalid: bool,
}

impl FixturePolicy {
    /// Establishes default-preserve responsibility before attempting to spawn.
    pub(super) fn reserve(&mut self) -> usize {
        let slot = self.slots.len();
        self.slots.push(false);
        slot
    }

    /// Records proof only for the exact reserved slot. Invalid slots poison the
    /// deletion decision rather than granting broad fixture cleanup authority.
    pub(super) fn settle(&mut self, slot: usize, evidence: Settlement) -> bool {
        let Some(settled) = self.slots.get_mut(slot) else {
            self.invalid = true;
            return false;
        };
        if *settled {
            self.invalid = true;
            return false;
        }
        if evidence.confirmed() {
            *settled = true;
            true
        } else {
            false
        }
    }

    /// Allows deletion only after every attempted owner has explicit proof.
    pub(super) fn can_delete(&self) -> bool {
        !self.invalid && !self.slots.is_empty() && self.slots.iter().all(|settled| *settled)
    }
}
