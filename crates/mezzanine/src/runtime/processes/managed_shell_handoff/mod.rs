//! Shell-neutral lifecycle ownership for managed interactive shell handoffs.
//!
//! Bash, Fish, and Zsh use different editor and startup APIs, but runtime PTY
//! ownership follows the same invariants: generated input is correlated to one
//! pane process and interaction epoch, exit text is not sent before a child is
//! proven, queued foreground input is not replayed before the original parent
//! is ready, and settlement occurs exactly once. This module models those
//! invariants independently from shell-specific wire protocols.
//!
//! The reducer is deliberately free of runtime I/O. Callers provide correlated
//! events and interpret declarative effects, which keeps stale or duplicated
//! adapter events from partially mutating transaction, rendering, or input
//! ownership outside one lifecycle decision.

use crate::runtime::PaneProcessInstance;

/// Maximum foreground input retained while a managed parent editor returns.
const MANAGED_SHELL_HANDOFF_INPUT_LIMIT_BYTES: usize = 64 * 1024;

/// Managed shell adapter that owns one interactive editor handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime) enum ManagedShellKind {
    /// GNU Bash Readline callback ownership.
    Bash,
    /// Fish command-line editor callback ownership.
    Fish,
    /// Zsh ZLE callback ownership.
    Zsh,
}

/// Runtime phase of one live managed-shell handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::runtime) enum ManagedShellHandoffPhase {
    /// The private editor trigger was written, but payload delivery has not begun.
    TriggerQueued,
    /// The native editor saved user state and admitted one correlated header.
    EditorHeld,
    /// The adapter admitted the private frame and payload delivery is in flight.
    PayloadInFlight,
    /// The persistent child authenticated that it owns terminal input.
    ChildInstalled,
    /// Cancellation or child exit was requested and the parent is returning.
    Returning,
    /// The child-exit boundary arrived while the adapter restores its editor.
    ParentRestoring,
    /// The parent-ready event was lost and exact foreground proof is pending.
    AwaitingParentProof,
    /// The original parent was authenticated and settlement may complete.
    ParentReady,
    /// All handoff ownership was released exactly once.
    Settled,
}

/// Exact identity that fences one handoff from stale pane or shell events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ManagedShellHandoffIdentity {
    /// Bootstrap marker authenticating private adapter records.
    pub(super) marker: String,
    /// Exact async-owned pane process generation, when ownership is detached.
    pub(super) process_instance: Option<PaneProcessInstance>,
    /// Original parent shell process that saved the editor state.
    pub(super) primary_process_id: Option<u32>,
    /// Shell interaction epoch that launched the child handoff.
    pub(super) interaction_generation: Option<u64>,
    /// Parent-only proof required when the adapter publishes readiness.
    pub(super) parent_proof: Option<String>,
}

/// Exact foreground observation that may prove the original parent returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::runtime) struct ManagedShellRecoveryObservation {
    /// Adapter-owned process generation that must answer.
    pub(super) instance: PaneProcessInstance,
    /// Opaque correlation id required on the worker result.
    pub(super) observation_id: String,
    /// Time when the exact worker observation was requested.
    pub(super) started_at_unix_ms: u64,
}

/// Terminal reason for releasing one managed-shell handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedShellHandoffOutcome {
    /// The adapter emitted its authenticated parent-ready event.
    ParentReady,
    /// Fresh foreground proof recovered a lost parent-ready event.
    ForegroundProof,
    /// The owning pane process was removed and queued input was discarded.
    PaneRemoved,
}

/// Shell-neutral aggregate owning one managed interactive handoff.
#[derive(Clone, Debug)]
pub(super) struct ManagedShellHandoff {
    /// Native adapter responsible for editor save and restoration.
    shell: ManagedShellKind,
    /// Identity fencing every lifecycle event.
    identity: ManagedShellHandoffIdentity,
    /// Current reducer-owned lifecycle phase.
    phase: ManagedShellHandoffPhase,
    /// Exit intent retained independently from payload and child phases.
    exit_requested: bool,
    /// Time when return or recovery waiting began.
    started_at_unix_ms: Option<u64>,
    /// Fresh foreground proof currently owned by this handoff.
    recovery_observation: Option<ManagedShellRecoveryObservation>,
    /// Foreground bytes withheld until parent ownership is authenticated.
    pending_input: Vec<u8>,
    /// Exact-once terminal settlement outcome.
    outcome: Option<ManagedShellHandoffOutcome>,
}

impl ManagedShellHandoff {
    /// Creates one trigger-owned handoff for an exact parent identity.
    pub(super) fn new(shell: ManagedShellKind, identity: ManagedShellHandoffIdentity) -> Self {
        Self {
            shell,
            identity,
            phase: ManagedShellHandoffPhase::TriggerQueued,
            exit_requested: false,
            started_at_unix_ms: None,
            recovery_observation: None,
            pending_input: Vec::new(),
            outcome: None,
        }
    }

    /// Returns the native adapter owning this handoff.
    pub(super) fn shell(&self) -> ManagedShellKind {
        self.shell
    }

    /// Returns the exact handoff identity.
    pub(super) fn identity(&self) -> &ManagedShellHandoffIdentity {
        &self.identity
    }

    /// Returns the current reducer-owned phase in invariant tests.
    #[cfg(test)]
    pub(super) fn phase(&self) -> ManagedShellHandoffPhase {
        self.phase
    }

    /// Returns when return or proof recovery waiting began.
    pub(super) fn started_at_unix_ms(&self) -> Option<u64> {
        self.started_at_unix_ms
    }

    /// Returns the exact foreground observation currently in flight.
    pub(super) fn recovery_observation(&self) -> Option<&ManagedShellRecoveryObservation> {
        self.recovery_observation.as_ref()
    }

    /// Reports whether exit was requested before the handoff settled.
    pub(super) fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    /// Returns queued foreground input without releasing ownership in tests.
    #[cfg(test)]
    pub(super) fn pending_input(&self) -> &[u8] {
        &self.pending_input
    }

    /// Returns whether this aggregate has settled exactly once.
    pub(super) fn is_settled(&self) -> bool {
        self.phase == ManagedShellHandoffPhase::Settled
    }
}

/// Correlated lifecycle input accepted by the pure handoff reducer.
#[derive(Clone, Debug)]
pub(super) enum ManagedShellHandoffEvent {
    /// The native adapter saved and cleared its editor for this marker.
    EditorHeld { marker: String },
    /// The adapter admitted the private frame and runtime released its payload.
    PayloadReleased { marker: String },
    /// The persistent child authenticated terminal-input ownership.
    ChildInstalled { marker: String, now_unix_ms: u64 },
    /// The user requested exit at the current lifecycle phase.
    ExitRequested { now_unix_ms: u64 },
    /// Runtime sent authenticated pre-payload cancellation.
    CancellationSent { now_unix_ms: u64 },
    /// Transaction transport failed before lifecycle completion was proven.
    TransportFailed { marker: String, now_unix_ms: u64 },
    /// The generic child-exit rendering boundary was observed.
    ChildExitBoundary,
    /// Foreground input arrived while parent ownership was uncertain.
    QueueInput { bytes: Vec<u8> },
    /// Runtime requested exact foreground proof after a lost return event.
    RecoveryProofRequested {
        observation: ManagedShellRecoveryObservation,
    },
    /// Foreground proof could not be requested for the owning process.
    RecoveryProofUnavailable,
    /// Foreground proof did not establish original-parent ownership.
    RecoveryProofRejected { now_unix_ms: u64 },
    /// The adapter emitted an authenticated parent-ready event.
    ParentReady {
        identity: ManagedShellHandoffIdentity,
    },
    /// Foreground proof established original-parent ownership.
    RecoveryProofAccepted {
        identity: ManagedShellHandoffIdentity,
        instance: PaneProcessInstance,
        observation_id: String,
    },
    /// The owning pane process was removed or replaced.
    PaneRemoved,
}

/// Declarative action selected by one reducer transition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ManagedShellHandoffEffect {
    /// Send authenticated cancellation before payload delivery begins.
    CancelBeforePayload,
    /// Retain exit intent until child installation proves a safe reader.
    WaitForChildInstallation,
    /// Send one generation-fenced exit request to the proven child.
    ExitChild,
    /// Arm or refresh the bounded parent-return watchdog.
    ArmWatchdog,
    /// Request exact foreground proof for the original parent process.
    RequestParentProof,
    /// Release all ownership and optionally replay authenticated parent input.
    Settle {
        /// Terminal settlement reason.
        outcome: ManagedShellHandoffOutcome,
        /// Queued bytes safe to replay; pane removal always returns no bytes.
        pending_input: Vec<u8>,
    },
}

/// Result of applying one event to the handoff reducer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ManagedShellHandoffTransition {
    /// Whether the event matched the current identity and phase.
    pub(super) applied: bool,
    /// Ordered effects selected by the transition.
    pub(super) effects: Vec<ManagedShellHandoffEffect>,
}

/// Applies one correlated event to a managed-shell handoff.
///
/// Invalid, stale, duplicated, and out-of-order events are inert. The caller
/// may log them, but must not infer cleanup from a rejected event.
pub(super) fn reduce_managed_shell_handoff(
    handoff: &mut ManagedShellHandoff,
    event: ManagedShellHandoffEvent,
) -> ManagedShellHandoffTransition {
    if handoff.is_settled() {
        return ManagedShellHandoffTransition::default();
    }
    let mut transition = ManagedShellHandoffTransition {
        applied: true,
        effects: Vec::new(),
    };
    match event {
        ManagedShellHandoffEvent::EditorHeld { marker }
            if marker == handoff.identity.marker
                && handoff.phase == ManagedShellHandoffPhase::TriggerQueued =>
        {
            handoff.phase = ManagedShellHandoffPhase::EditorHeld;
        }
        ManagedShellHandoffEvent::PayloadReleased { marker }
            if marker == handoff.identity.marker
                && matches!(
                    handoff.phase,
                    ManagedShellHandoffPhase::TriggerQueued | ManagedShellHandoffPhase::EditorHeld
                ) =>
        {
            handoff.phase = ManagedShellHandoffPhase::PayloadInFlight;
        }
        ManagedShellHandoffEvent::ChildInstalled {
            marker,
            now_unix_ms,
        } if marker == handoff.identity.marker
            && handoff.phase == ManagedShellHandoffPhase::PayloadInFlight =>
        {
            handoff.phase = ManagedShellHandoffPhase::ChildInstalled;
            if handoff.exit_requested {
                handoff.phase = ManagedShellHandoffPhase::Returning;
                handoff.started_at_unix_ms = Some(now_unix_ms);
                transition
                    .effects
                    .push(ManagedShellHandoffEffect::ExitChild);
                transition
                    .effects
                    .push(ManagedShellHandoffEffect::ArmWatchdog);
            }
        }
        ManagedShellHandoffEvent::ExitRequested { now_unix_ms } => {
            handoff.exit_requested = true;
            match handoff.phase {
                ManagedShellHandoffPhase::TriggerQueued | ManagedShellHandoffPhase::EditorHeld => {
                    transition
                        .effects
                        .push(ManagedShellHandoffEffect::CancelBeforePayload)
                }
                ManagedShellHandoffPhase::PayloadInFlight => transition
                    .effects
                    .push(ManagedShellHandoffEffect::WaitForChildInstallation),
                ManagedShellHandoffPhase::ChildInstalled => {
                    handoff.phase = ManagedShellHandoffPhase::Returning;
                    handoff.started_at_unix_ms = Some(now_unix_ms);
                    transition
                        .effects
                        .push(ManagedShellHandoffEffect::ExitChild);
                    transition
                        .effects
                        .push(ManagedShellHandoffEffect::ArmWatchdog);
                }
                ManagedShellHandoffPhase::Returning
                | ManagedShellHandoffPhase::ParentRestoring
                | ManagedShellHandoffPhase::AwaitingParentProof
                | ManagedShellHandoffPhase::ParentReady => {}
                ManagedShellHandoffPhase::Settled => unreachable!(),
            }
        }
        ManagedShellHandoffEvent::CancellationSent { now_unix_ms }
            if matches!(
                handoff.phase,
                ManagedShellHandoffPhase::TriggerQueued | ManagedShellHandoffPhase::EditorHeld
            ) && handoff.exit_requested =>
        {
            handoff.phase = ManagedShellHandoffPhase::Returning;
            handoff.started_at_unix_ms = Some(now_unix_ms);
            transition
                .effects
                .push(ManagedShellHandoffEffect::ArmWatchdog);
        }
        ManagedShellHandoffEvent::TransportFailed {
            marker,
            now_unix_ms,
        } if marker == handoff.identity.marker => {
            handoff.phase = ManagedShellHandoffPhase::Returning;
            handoff.started_at_unix_ms = Some(now_unix_ms);
            handoff.recovery_observation = None;
            transition
                .effects
                .push(ManagedShellHandoffEffect::ArmWatchdog);
        }
        ManagedShellHandoffEvent::ChildExitBoundary
            if matches!(
                handoff.phase,
                ManagedShellHandoffPhase::ChildInstalled | ManagedShellHandoffPhase::Returning
            ) =>
        {
            handoff.phase = ManagedShellHandoffPhase::ParentRestoring;
        }
        ManagedShellHandoffEvent::QueueInput { bytes } => {
            if handoff.pending_input.len().saturating_add(bytes.len())
                > MANAGED_SHELL_HANDOFF_INPUT_LIMIT_BYTES
            {
                transition.applied = false;
            } else {
                handoff.pending_input.extend_from_slice(&bytes);
            }
        }
        ManagedShellHandoffEvent::RecoveryProofRequested { observation }
            if matches!(
                handoff.phase,
                ManagedShellHandoffPhase::Returning
                    | ManagedShellHandoffPhase::ParentRestoring
                    | ManagedShellHandoffPhase::AwaitingParentProof
            ) =>
        {
            handoff.phase = ManagedShellHandoffPhase::AwaitingParentProof;
            handoff.started_at_unix_ms = None;
            handoff.recovery_observation = Some(observation);
            transition
                .effects
                .push(ManagedShellHandoffEffect::RequestParentProof);
        }
        ManagedShellHandoffEvent::RecoveryProofUnavailable
            if matches!(
                handoff.phase,
                ManagedShellHandoffPhase::Returning
                    | ManagedShellHandoffPhase::ParentRestoring
                    | ManagedShellHandoffPhase::AwaitingParentProof
            ) =>
        {
            handoff.phase = ManagedShellHandoffPhase::AwaitingParentProof;
            handoff.started_at_unix_ms = None;
            handoff.recovery_observation = None;
        }
        ManagedShellHandoffEvent::RecoveryProofRejected { now_unix_ms }
            if handoff.phase == ManagedShellHandoffPhase::AwaitingParentProof =>
        {
            handoff.phase = ManagedShellHandoffPhase::Returning;
            handoff.started_at_unix_ms = Some(now_unix_ms);
            handoff.recovery_observation = None;
            transition
                .effects
                .push(ManagedShellHandoffEffect::ArmWatchdog);
        }
        ManagedShellHandoffEvent::ParentReady { identity }
            if identity == handoff.identity
                && matches!(
                    handoff.phase,
                    ManagedShellHandoffPhase::TriggerQueued
                        | ManagedShellHandoffPhase::EditorHeld
                        | ManagedShellHandoffPhase::PayloadInFlight
                        | ManagedShellHandoffPhase::ChildInstalled
                        | ManagedShellHandoffPhase::Returning
                        | ManagedShellHandoffPhase::ParentRestoring
                        | ManagedShellHandoffPhase::AwaitingParentProof
                ) =>
        {
            settle_handoff(
                handoff,
                ManagedShellHandoffOutcome::ParentReady,
                &mut transition,
                true,
            );
        }
        ManagedShellHandoffEvent::RecoveryProofAccepted {
            identity,
            instance,
            observation_id,
        } if identity == handoff.identity
            && handoff.phase == ManagedShellHandoffPhase::AwaitingParentProof
            && handoff
                .recovery_observation
                .as_ref()
                .is_some_and(|pending| {
                    pending.instance == instance && pending.observation_id == observation_id
                }) =>
        {
            settle_handoff(
                handoff,
                ManagedShellHandoffOutcome::ForegroundProof,
                &mut transition,
                true,
            );
        }
        ManagedShellHandoffEvent::PaneRemoved => {
            settle_handoff(
                handoff,
                ManagedShellHandoffOutcome::PaneRemoved,
                &mut transition,
                false,
            );
        }
        _ => transition.applied = false,
    }
    transition
}

/// Settles one aggregate and emits the only effect allowed to release input.
fn settle_handoff(
    handoff: &mut ManagedShellHandoff,
    outcome: ManagedShellHandoffOutcome,
    transition: &mut ManagedShellHandoffTransition,
    replay_input: bool,
) {
    handoff.phase = ManagedShellHandoffPhase::ParentReady;
    let pending_input = if replay_input {
        std::mem::take(&mut handoff.pending_input)
    } else {
        handoff.pending_input.clear();
        Vec::new()
    };
    handoff.started_at_unix_ms = None;
    handoff.recovery_observation = None;
    handoff.outcome = Some(outcome);
    handoff.phase = ManagedShellHandoffPhase::Settled;
    transition.effects.push(ManagedShellHandoffEffect::Settle {
        outcome,
        pending_input,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds one exact adapter-owned identity for reducer invariants.
    fn identity(marker: &str) -> ManagedShellHandoffIdentity {
        ManagedShellHandoffIdentity {
            marker: marker.to_string(),
            process_instance: Some(PaneProcessInstance {
                pane_id: "%1".to_string(),
                generation: 7,
            }),
            primary_process_id: Some(41),
            interaction_generation: Some(11),
            parent_proof: None,
        }
    }

    /// Verifies exit before payload delivery uses authenticated cancellation
    /// and releases queued input only after the exact parent-ready identity.
    #[test]
    fn pre_payload_exit_cancels_and_settles_after_exact_parent_ready() {
        let expected = identity("marker-1");
        let mut handoff = ManagedShellHandoff::new(ManagedShellKind::Fish, expected.clone());

        let exit = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 10 },
        );
        assert_eq!(
            exit.effects,
            vec![ManagedShellHandoffEffect::CancelBeforePayload]
        );
        let cancelled = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::CancellationSent { now_unix_ms: 11 },
        );
        assert_eq!(handoff.phase(), ManagedShellHandoffPhase::Returning);
        assert_eq!(
            cancelled.effects,
            vec![ManagedShellHandoffEffect::ArmWatchdog]
        );
        assert!(
            reduce_managed_shell_handoff(
                &mut handoff,
                ManagedShellHandoffEvent::QueueInput {
                    bytes: b"queued\n".to_vec(),
                },
            )
            .applied
        );
        let settled = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::ParentReady { identity: expected },
        );

        assert_eq!(
            settled.effects,
            vec![ManagedShellHandoffEffect::Settle {
                outcome: ManagedShellHandoffOutcome::ParentReady,
                pending_input: b"queued\n".to_vec(),
            }]
        );
        assert!(handoff.is_settled());
    }

    /// Verifies exit during payload delivery is retained without terminal text
    /// until authenticated child installation proves a safe input reader.
    #[test]
    fn payload_exit_waits_for_child_installation_before_exit_effect() {
        let mut handoff = ManagedShellHandoff::new(ManagedShellKind::Zsh, identity("marker-2"));
        assert!(
            reduce_managed_shell_handoff(
                &mut handoff,
                ManagedShellHandoffEvent::PayloadReleased {
                    marker: "marker-2".to_string(),
                },
            )
            .effects
            .is_empty()
        );
        let exit = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 20 },
        );
        assert_eq!(
            exit.effects,
            vec![ManagedShellHandoffEffect::WaitForChildInstallation]
        );

        let installed = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::ChildInstalled {
                marker: "marker-2".to_string(),
                now_unix_ms: 21,
            },
        );
        assert_eq!(
            installed.effects,
            vec![
                ManagedShellHandoffEffect::ExitChild,
                ManagedShellHandoffEffect::ArmWatchdog,
            ]
        );
        assert_eq!(handoff.phase(), ManagedShellHandoffPhase::Returning);
    }

    /// Verifies editor-held exit and proven-child exit select disjoint effects,
    /// while a duplicate request during return cannot emit a second child exit.
    #[test]
    fn exit_phase_matrix_cancels_before_data_and_exits_proven_child_once() {
        let mut editor_held =
            ManagedShellHandoff::new(ManagedShellKind::Fish, identity("marker-editor"));
        assert!(
            reduce_managed_shell_handoff(
                &mut editor_held,
                ManagedShellHandoffEvent::EditorHeld {
                    marker: "marker-editor".to_string(),
                },
            )
            .applied
        );
        let cancelled = reduce_managed_shell_handoff(
            &mut editor_held,
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 22 },
        );
        assert_eq!(
            cancelled.effects,
            vec![ManagedShellHandoffEffect::CancelBeforePayload]
        );

        let mut installed =
            ManagedShellHandoff::new(ManagedShellKind::Zsh, identity("marker-child"));
        let _ = reduce_managed_shell_handoff(
            &mut installed,
            ManagedShellHandoffEvent::PayloadReleased {
                marker: "marker-child".to_string(),
            },
        );
        let _ = reduce_managed_shell_handoff(
            &mut installed,
            ManagedShellHandoffEvent::ChildInstalled {
                marker: "marker-child".to_string(),
                now_unix_ms: 23,
            },
        );
        let exited = reduce_managed_shell_handoff(
            &mut installed,
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 24 },
        );
        assert_eq!(
            exited.effects,
            vec![
                ManagedShellHandoffEffect::ExitChild,
                ManagedShellHandoffEffect::ArmWatchdog,
            ]
        );
        let duplicate = reduce_managed_shell_handoff(
            &mut installed,
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 25 },
        );
        assert!(duplicate.applied);
        assert!(duplicate.effects.is_empty());
        assert_eq!(installed.phase(), ManagedShellHandoffPhase::Returning);
    }

    /// Verifies stale markers and process generations cannot advance or settle
    /// the live handoff and therefore cannot release queued foreground input.
    #[test]
    fn stale_identity_events_are_inert() {
        let expected = identity("marker-3");
        let mut handoff = ManagedShellHandoff::new(ManagedShellKind::Fish, expected.clone());
        assert!(
            !reduce_managed_shell_handoff(
                &mut handoff,
                ManagedShellHandoffEvent::PayloadReleased {
                    marker: "stale".to_string(),
                },
            )
            .applied
        );
        let mut stale = expected;
        stale.process_instance.as_mut().unwrap().generation += 1;
        assert!(
            !reduce_managed_shell_handoff(
                &mut handoff,
                ManagedShellHandoffEvent::ParentReady { identity: stale },
            )
            .applied
        );
        assert_eq!(handoff.phase(), ManagedShellHandoffPhase::TriggerQueued);
    }

    /// Verifies a timeout and rejected proof retain queued bytes, while only a
    /// matching foreground observation can authorize replay and settlement.
    #[test]
    fn foreground_proof_gates_recovery_input_replay() {
        let expected = identity("marker-4");
        let instance = expected.process_instance.clone().unwrap();
        let mut handoff = ManagedShellHandoff::new(ManagedShellKind::Fish, expected.clone());
        let _ = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 30 },
        );
        let _ = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::CancellationSent { now_unix_ms: 31 },
        );
        let _ = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::QueueInput {
                bytes: b"retained".to_vec(),
            },
        );
        let observation = ManagedShellRecoveryObservation {
            instance: instance.clone(),
            observation_id: "proof-1".to_string(),
            started_at_unix_ms: 40,
        };
        let requested = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::RecoveryProofRequested { observation },
        );
        assert_eq!(
            requested.effects,
            vec![ManagedShellHandoffEffect::RequestParentProof]
        );
        let rejected = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::RecoveryProofRejected { now_unix_ms: 41 },
        );
        assert_eq!(
            rejected.effects,
            vec![ManagedShellHandoffEffect::ArmWatchdog]
        );
        assert_eq!(handoff.pending_input(), b"retained");
        let _ = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::RecoveryProofRequested {
                observation: ManagedShellRecoveryObservation {
                    instance: instance.clone(),
                    observation_id: "proof-2".to_string(),
                    started_at_unix_ms: 50,
                },
            },
        );
        let settled = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::RecoveryProofAccepted {
                identity: expected,
                instance,
                observation_id: "proof-2".to_string(),
            },
        );
        assert_eq!(
            settled.effects,
            vec![ManagedShellHandoffEffect::Settle {
                outcome: ManagedShellHandoffOutcome::ForegroundProof,
                pending_input: b"retained".to_vec(),
            }]
        );
    }

    /// Verifies transport failure never injects exit or releases queued input,
    /// and instead arms proof-gated parent recovery for the exact marker.
    #[test]
    fn transport_failure_enters_proof_gated_return_without_input_effects() {
        let mut handoff = ManagedShellHandoff::new(ManagedShellKind::Fish, identity("marker-5"));
        let _ = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::QueueInput {
                bytes: b"retained".to_vec(),
            },
        );

        let stale = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::TransportFailed {
                marker: "stale".to_string(),
                now_unix_ms: 60,
            },
        );
        assert!(!stale.applied);
        assert_eq!(handoff.phase(), ManagedShellHandoffPhase::TriggerQueued);

        let failed = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::TransportFailed {
                marker: "marker-5".to_string(),
                now_unix_ms: 61,
            },
        );
        assert_eq!(failed.effects, vec![ManagedShellHandoffEffect::ArmWatchdog]);
        assert_eq!(handoff.phase(), ManagedShellHandoffPhase::Returning);
        assert_eq!(handoff.pending_input(), b"retained");
    }

    /// Verifies settlement is exact once and pane removal discards unsafe input
    /// rather than replaying bytes to an unproven or replacement process.
    #[test]
    fn settlement_is_exact_once_and_pane_removal_discards_input() {
        let expected = identity("marker-6");
        let mut handoff = ManagedShellHandoff::new(ManagedShellKind::Zsh, expected.clone());
        let _ = reduce_managed_shell_handoff(
            &mut handoff,
            ManagedShellHandoffEvent::QueueInput {
                bytes: b"unsafe".to_vec(),
            },
        );
        let removed =
            reduce_managed_shell_handoff(&mut handoff, ManagedShellHandoffEvent::PaneRemoved);
        assert_eq!(
            removed.effects,
            vec![ManagedShellHandoffEffect::Settle {
                outcome: ManagedShellHandoffOutcome::PaneRemoved,
                pending_input: Vec::new(),
            }]
        );
        for duplicate in [
            ManagedShellHandoffEvent::PaneRemoved,
            ManagedShellHandoffEvent::ParentReady {
                identity: expected.clone(),
            },
            ManagedShellHandoffEvent::ExitRequested { now_unix_ms: 99 },
        ] {
            let transition = reduce_managed_shell_handoff(&mut handoff, duplicate);
            assert!(!transition.applied);
            assert!(transition.effects.is_empty());
        }
    }
}
