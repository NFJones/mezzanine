//! Contracts for trusted, process-free native semantic operations.
//!
//! These contracts describe the target adapter, not proof that a product
//! executor implements it. They own no filesystem, process, or authority store.
//! A product adapter must resolve current trust/scopes, bind exact approval
//! evidence, and fence each commit before reporting a confirmed effect. Runtime
//! filesystem capabilities are not OS confinement of the hosting daemon.

use crate::local_action::{LocalActionKind, LocalActionPlanningError};
use crate::semantic_patch::{MezPatch, MezPatchOperation, parse_mez_patch};

/// Execution adapter required by the native semantic contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredLocalAdapter {
    /// Existing interactive/remote pane shell adapter.
    PaneShell,
    /// Fresh shell with the existing environment and OS sandbox lifecycle.
    SpawnedShell,
    /// In-process Rust/OS filesystem implementation, without child launches.
    NativeRuntime,
}

impl RequiredLocalAdapter {
    /// Selects the required adapter before any shell source is generated.
    /// This is a migration contract; it must not relabel legacy shell output.
    pub const fn select(kind: LocalActionKind, native: bool) -> Self {
        match (native, kind) {
            (false, _) => Self::PaneShell,
            (true, LocalActionKind::ShellCommand) => Self::SpawnedShell,
            (true, LocalActionKind::ApplyPatch) => Self::NativeRuntime,
        }
    }

    /// Returns the truthful result transport name for this adapter.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PaneShell => "pane_shell",
            Self::SpawnedShell => "spawned_shell",
            Self::NativeRuntime => "native_runtime",
        }
    }

    /// Reports whether this adapter may send execution input to the pane PTY.
    pub const fn sent_to_pane(self) -> bool {
        matches!(self, Self::PaneShell)
    }
}

/// Ordered semantic effect; paths are syntax, not resolved filesystem grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchEffect {
    /// Create a missing destination, never replace a competing create.
    Create(String),
    /// Read and replace an exact captured preimage.
    Update(String),
    /// Read and remove an exact captured object.
    Delete(String),
    /// Publish destination before removing source; endpoints stay dependent.
    Move { source: String, destination: String },
}

/// Shell-free patch descriptor; the policy label is not executable input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePatchOperation {
    /// Exact accepted payload, retained separately from parsed/normalized text.
    pub exact_patch: String,
    /// Shared deterministic parser representation.
    pub patch: MezPatch,
    /// Ordered effects, including dependent move endpoints.
    pub effects: Vec<PatchEffect>,
}

impl NativePatchOperation {
    /// Parses an accepted patch without shell lowering or filesystem access.
    /// Rejects unsupported stripping rather than changing approval identity.
    pub fn parse(patch: &str, strip: Option<u64>) -> Result<Self, LocalActionPlanningError> {
        if strip.is_some() {
            return Err(LocalActionPlanningError::new(
                "apply_patch strip is unsupported for Mezzanine patch blocks",
            ));
        }
        let parsed = parse_mez_patch(patch)
            .map_err(|error| LocalActionPlanningError::new(format!("apply_patch: {error}")))?;
        let effects = parsed
            .operations
            .iter()
            .map(|operation| match operation {
                MezPatchOperation::Add { path, .. } => PatchEffect::Create(path.clone()),
                MezPatchOperation::Delete { path } => PatchEffect::Delete(path.clone()),
                MezPatchOperation::Update {
                    path,
                    move_to: None,
                    ..
                } => PatchEffect::Update(path.clone()),
                MezPatchOperation::Update {
                    path,
                    move_to: Some(destination),
                    ..
                } => PatchEffect::Move {
                    source: path.clone(),
                    destination: destination.clone(),
                },
            })
            .collect();
        Ok(Self {
            exact_patch: patch.to_string(),
            patch: parsed,
            effects,
        })
    }

    /// Classifier identity only; never execute this string as a program.
    pub const fn policy_descriptor(&self) -> &'static str {
        "apply_patch"
    }

    /// Finite patch budget, capped again by the current turn deadline.
    pub const fn timeout_ms(&self) -> u64 {
        30_000
    }
}

/// Exact commit authorization evidence supplied by the existing actor owner.
/// Equality binds approval to payload, effects, transaction and authority;
/// paths alone, policy labels, or normalized command hashes are insufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCommitIdentity {
    /// Current owning turn.
    pub turn_id: String,
    /// Current owning action.
    pub action_id: String,
    /// Exact transaction generation, distinct from display/progress revisions.
    pub transaction: String,
    /// Current trust/permission generation, rechecked before each publication.
    pub authority_generation: u64,
    /// Exact semantic operation and ordered effects accepted by policy.
    pub operation: NativePatchOperation,
}

/// Phase identity independent of shell markers and shell stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePatchPhase {
    /// Bounded descriptor-relative snapshots, with explicit read authority.
    Snapshot,
    /// Pure matching and bounded native diff generation.
    Plan,
    /// Exclusive transaction-owned staging, with cleanup guards.
    Stage,
    /// Actor-authorized publication; cancellation cannot undo a started syscall.
    Commit,
    /// Attribute confirmed/partial/unknown effects without replay.
    Settle,
}

/// Filesystem effect certainty, never inferred from deadline or worker drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeEffectCertainty {
    /// Commits are fenced off and no mutation began.
    Unexecuted,
    /// Every reported effect has positive completion evidence.
    Confirmed,
    /// Earlier effects are confirmed and later operations failed or stopped.
    Partial,
    /// A started filesystem call has not yet produced completion evidence.
    InFlight,
    /// Completion evidence is unavailable; automatic replay is forbidden.
    Unknown,
}

/// Typed native result, intentionally without shell exit/signal/stdout fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativePatchOutcome {
    /// Exact actor/transaction identity retained even after cancellation.
    pub identity: NativeCommitIdentity,
    /// Last positively observed phase.
    pub phase: NativePatchPhase,
    /// Truthful certainty for the whole operation.
    pub certainty: NativeEffectCertainty,
    /// Confirmed ordered effects; presentation truncation must not truncate this.
    pub confirmed_effects: Vec<PatchEffect>,
    /// Bounded model-safe failure/uncertainty diagnostic, if any.
    pub diagnostic: Option<String>,
}

/// Actual protection provided by a trusted in-process filesystem adapter.
/// It does not confine the daemon or provide namespace/network isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeFilesystemEnforcement {
    /// Descriptor-relative operations checked against current actor authority.
    RuntimeCapabilities,
}

impl NativeFilesystemEnforcement {
    /// Admits this adapter only when runtime filesystem capabilities satisfy
    /// the required protection. Mandatory OS process confinement is incompatible
    /// with this adapter and fails closed, never as an implicit approved bypass.
    pub fn admit(requires_os_process_confinement: bool) -> Result<Self, LocalActionPlanningError> {
        if requires_os_process_confinement {
            return Err(LocalActionPlanningError::new(
                "native filesystem capabilities cannot provide mandatory OS process confinement",
            ));
        }
        Ok(Self::RuntimeCapabilities)
    }

    /// Stable actual-enforcement metadata, distinct from configured backend intent.
    pub const fn as_str(self) -> &'static str {
        "runtime_filesystem_capabilities"
    }
}

impl NativeEffectCertainty {
    /// Classifies cooperative cancellation from positive worker/commit evidence.
    /// An acknowledged fence prevents future commits but cannot stop a syscall
    /// already in flight. Lost evidence or an unacknowledged fence is unknown,
    /// even if the actor's deadline has passed or its worker handle was dropped.
    /// Complete evidence means every earlier commit attempt is accounted for;
    /// it is independent of acknowledging the fence against future commits.
    pub const fn after_cancellation(
        commit_fence_acknowledged: bool,
        commit_in_flight: bool,
        has_confirmed_effects: bool,
        completion_evidence_complete: bool,
    ) -> Self {
        if commit_in_flight {
            Self::InFlight
        } else if !commit_fence_acknowledged || !completion_evidence_complete {
            Self::Unknown
        } else if has_confirmed_effects {
            Self::Partial
        } else {
            Self::Unexecuted
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runtime capabilities never claim OS confinement or silently reinterpret
    /// a mandatory sandbox requirement as a bypass authorization.
    #[test]
    fn native_enforcement_fails_closed_on_incompatible_requirement() {
        assert!(NativeFilesystemEnforcement::admit(true).is_err());
        assert_eq!(
            NativeFilesystemEnforcement::admit(false).unwrap().as_str(),
            "runtime_filesystem_capabilities"
        );
    }

    /// A timeout alone proves nothing about effects; acknowledged cancellation
    /// distinguishes stopped, partial, and already-started publication work.
    #[test]
    fn cancellation_certainty_requires_positive_commit_evidence() {
        use NativeEffectCertainty as Certainty;
        assert_eq!(
            Certainty::after_cancellation(false, false, false, true),
            Certainty::Unknown
        );
        assert_eq!(
            Certainty::after_cancellation(true, true, false, true),
            Certainty::InFlight
        );
        assert_eq!(
            Certainty::after_cancellation(true, false, false, true),
            Certainty::Unexecuted
        );
        assert_eq!(
            Certainty::after_cancellation(true, false, true, true),
            Certainty::Partial
        );
        assert_eq!(
            Certainty::after_cancellation(false, false, true, true),
            Certainty::Unknown
        );
    }

    /// Acknowledging the future-commit fence cannot reconstruct lost evidence
    /// about an earlier publication, with or without confirmed prior effects.
    #[test]
    fn acknowledged_cancellation_with_lost_completion_evidence_is_unknown() {
        for has_confirmed_effects in [false, true] {
            assert_eq!(
                NativeEffectCertainty::after_cancellation(
                    true,
                    false,
                    has_confirmed_effects,
                    false
                ),
                NativeEffectCertainty::Unknown
            );
        }
    }

    /// Pins all four local-action/mode combinations without invoking adapters.
    #[test]
    fn required_adapter_matrix_is_explicit() {
        assert_eq!(
            RequiredLocalAdapter::select(LocalActionKind::ApplyPatch, false),
            RequiredLocalAdapter::PaneShell
        );
        assert_eq!(
            RequiredLocalAdapter::select(LocalActionKind::ShellCommand, false),
            RequiredLocalAdapter::PaneShell
        );
        assert_eq!(
            RequiredLocalAdapter::select(LocalActionKind::ShellCommand, true),
            RequiredLocalAdapter::SpawnedShell
        );
        let native = RequiredLocalAdapter::select(LocalActionKind::ApplyPatch, true);
        assert_eq!(native.as_str(), "native_runtime");
        assert!(!native.sent_to_pane());
    }

    /// Keeps exact CRLF payload and ordered move dependency separate from the
    /// parser's matching normalization; unsupported strip fails before I/O.
    #[test]
    fn semantic_descriptor_preserves_exact_payload_and_moves() {
        let source = "*** Begin Patch\r\n*** Update File: old\r\n*** Move to: new\r\n@@\r\n-a\r\n+b\r\n*** End Patch\r\n";
        let operation = NativePatchOperation::parse(source, None).unwrap();
        assert_eq!(operation.exact_patch, source);
        assert_eq!(
            operation.effects,
            vec![PatchEffect::Move {
                source: "old".into(),
                destination: "new".into()
            }]
        );
        assert_eq!(operation.policy_descriptor(), "apply_patch");
        assert_eq!(operation.timeout_ms(), 30_000);
        assert!(NativePatchOperation::parse(source, Some(0)).is_err());
        assert!(
            NativePatchOperation::parse(
                "*** Begin Patch\n*** Add File: ../escape\n+x\n*** End Patch",
                None
            )
            .is_err()
        );
    }

    /// Approval identity changes with exact source, transaction or authority,
    /// even when a normalized patch would derive the same effect paths.
    #[test]
    fn commit_identity_is_exact_and_generation_bound() {
        let source = "*** Begin Patch\n*** Add File: a\n+x\n*** End Patch\n";
        let identity = NativeCommitIdentity {
            turn_id: "t".into(),
            action_id: "a".into(),
            transaction: "g1".into(),
            authority_generation: 1,
            operation: NativePatchOperation::parse(source, None).unwrap(),
        };
        let mut changed = identity.clone();
        changed.authority_generation += 1;
        assert_ne!(identity, changed);
        changed = identity.clone();
        changed.transaction = "g2".into();
        assert_ne!(identity, changed);
        changed = identity.clone();
        changed.operation =
            NativePatchOperation::parse(&source.replace('\n', "\r\n"), None).unwrap();
        assert_ne!(identity, changed);
    }
}
