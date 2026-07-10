//! Agent tests for pane readiness behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies bootstrap runs after signature change before user prompt.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn bootstrap_runs_after_signature_change_before_user_prompt() {
    let first = test_env_signature("host", "user", "/bin/sh", "/repo");
    let second = test_env_signature("host", "user", "/bin/sh", "/repo/sub");

    let unchanged =
        decide_bootstrap_before_user_prompt(PaneReadinessState::Ready, Some(&first), Some(&first));
    let changed =
        decide_bootstrap_before_user_prompt(PaneReadinessState::Ready, Some(&first), Some(&second));
    let blocked =
        decide_bootstrap_before_user_prompt(PaneReadinessState::PasswordPrompt, Some(&first), None);

    assert!(!unchanged.should_bootstrap);
    assert!(changed.should_bootstrap);
    assert!(blocked.block_turn);
}

#[test]
/// Verifies readiness blocks probes when pane is not ready.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn readiness_blocks_probes_when_pane_is_not_ready() {
    let busy = readiness_decision(PaneReadinessState::Busy);
    let unknown = readiness_decision(PaneReadinessState::Unknown);
    let prompt_candidate = readiness_decision(PaneReadinessState::PromptCandidate);
    let probing = readiness_decision(PaneReadinessState::Probing);
    let ready = readiness_decision(PaneReadinessState::Ready);

    assert!(!busy.may_probe);
    assert!(!busy.may_send_agent_command);
    assert!(busy.stale_signature_allowed);
    assert!(unknown.may_probe);
    assert!(!unknown.may_send_agent_command);
    assert!(prompt_candidate.may_probe);
    assert!(!prompt_candidate.may_send_agent_command);
    assert!(!probing.may_probe);
    assert!(!probing.may_send_agent_command);
    assert!(ready.may_probe);
    assert!(ready.may_send_agent_command);
}

#[test]
/// Verifies readiness override requires warning ack and is one epoch only.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn readiness_override_requires_warning_ack_and_is_one_epoch_only() {
    let mut store = PaneReadinessOverrideStore::default();
    store.record_pending_probe("%1", "probe-1").unwrap();
    assert!(store.has_pending_probe("%1"));

    let error = store
        .mark_ready_for_epoch("%1", 7, "primary accepted uncertain shell boundary", false)
        .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(store.has_pending_probe("%1"));

    store
        .mark_ready_for_epoch("%1", 7, "primary accepted uncertain shell boundary", true)
        .unwrap();
    assert!(!store.has_pending_probe("%1"));
    assert!(store.allows_epoch("%1", 7));
    assert!(!store.allows_epoch("%1", 8));

    let consumed = store.consume_epoch("%1", 7).unwrap();
    assert_eq!(consumed.pane_id, "%1");
    assert!(!store.allows_epoch("%1", 7));
}

#[test]
/// Verifies readiness override revokes on safety boundary changes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn readiness_override_revokes_on_safety_boundary_changes() {
    let mut store = PaneReadinessOverrideStore::default();
    store
        .mark_ready_for_epoch("%1", 1, "manual override", true)
        .unwrap();

    let revoked = store
        .revoke(
            "%1",
            ReadinessOverrideRevocation::EnvironmentSignatureChanged,
        )
        .unwrap();

    assert_eq!(revoked.epoch, 1);
    assert!(!store.allows_epoch("%1", 1));
}

#[test]
/// Verifies pending readiness probes are cleared only by their active marker.
///
/// This regression scenario protects probe epoch ownership. A late completion
/// or timeout from a superseded probe must not clear the active probe marker
/// for the same pane, otherwise stale probe output can mutate readiness for the
/// wrong shell boundary.
fn readiness_pending_probe_requires_matching_marker() {
    let mut store = PaneReadinessOverrideStore::default();

    store.record_pending_probe("%1", "probe-a").unwrap();
    store.record_pending_probe("%1", "probe-b").unwrap();

    assert_eq!(store.pending_probe_marker("%1"), Some("probe-b"));
    assert!(!store.clear_pending_probe_if_matches("%1", "probe-a"));
    assert_eq!(store.pending_probe_marker("%1"), Some("probe-b"));
    assert!(store.clear_pending_probe_if_matches("%1", "probe-b"));
    assert!(!store.has_pending_probe("%1"));
}
