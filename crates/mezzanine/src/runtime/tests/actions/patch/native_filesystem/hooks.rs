//! Non-executable hook admission for actual opaque filesystem attempts.
//!
//! Real pending canonical slots and descriptor roots exercise the production
//! claim boundary. Adapter queues prove no executable work escapes admission;
//! these checks do not claim completed-action or durable settlement coverage.

use super::*;
use crate::integrations::hooks::{HookDefinition, HookInvocation, HookOnFailure};

/// Installs one program or focused-shell guard without replacing source epochs,
/// path grants or the original turn. The effect adapter exposes queued launches.
fn install_guard(fixture: &mut Fixture, required: bool, focused: bool) {
    fixture.service.use_hook_effect_adapter();
    fixture
        .service
        .integration
        .replace_hook_definitions(vec![HookDefinition {
            id: "filesystem-guard".into(),
            event: HookEvent::PreShellCommand,
            invocation: if focused {
                HookInvocation::FocusedShell {
                    command: "printf guard".into(),
                }
            } else {
                HookInvocation::Program {
                    command: "/missing/native-guard".into(),
                    args: Vec::new(),
                }
            },
            enabled: true,
            required,
            agent_hook: false,
            matcher_groups: Vec::new(),
            timeout_ms: None,
            on_failure: Some(HookOnFailure::Warn),
        }]);
}

/// A required program or focused-shell guard must reject an actual queued
/// attempt before issuance, even with warn policy. Positive unissued custody
/// stays removable and no hook or filesystem effect is produced.
#[test]
fn required_hook_rejects_filesystem_claim_before_issuance() {
    for focused in [false, true] {
        let mut fixture = Fixture::new();
        install_guard(&mut fixture, true, focused);
        let attempt = fixture.queue();
        let error = fixture
            .service
            .claim_native_filesystem_action(&attempt)
            .err()
            .unwrap();
        assert!(error.message().contains("filesystem-guard"));
        assert_eq!(
            fixture
                .service
                .filesystem_completion_phase_for_tests(&attempt)
                .unwrap(),
            "filesystem_queued"
        );
        let unissued = fixture
            .service
            .take_unissued_native_filesystem_owner(&attempt)
            .unwrap();
        assert_eq!(unissued.owner.action.id, fixture.action.id);
        assert!(
            fixture
                .service
                .drain_program_hook_transition()
                .side_effects
                .is_empty()
        );
        assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
        assert!(!fixture.root.join("file").exists());
    }
}

/// Optional warn-only handlers allow actual one-shot admission without queuing
/// executable work. A repeated claim cannot run the optional integration again
/// or issue a second dispatch for the same opaque occurrence.
#[test]
fn optional_hook_allows_filesystem_claim_without_executable_work() {
    for focused in [false, true] {
        let mut fixture = Fixture::new();
        install_guard(&mut fixture, false, focused);
        let attempt = fixture.queue();
        let dispatch = fixture
            .service
            .claim_native_filesystem_action(&attempt)
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .service
                .claim_native_filesystem_action(&attempt)
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .service
                .drain_program_hook_transition()
                .side_effects
                .is_empty()
        );
        assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
        assert!(!fixture.root.join("file").exists());
        dispatch.fence.cancel();
    }
}

/// The explicit filesystem domain remains non-executable under pane-mode
/// presentation and shell-shaped payloads. A caller's registered execution
/// domain, rather than mutable pane state or payload labels, owns this gate.
#[test]
fn filesystem_hook_domain_does_not_adopt_current_pane_mode() {
    let mut fixture = Fixture::new();
    install_guard(&mut fixture, true, false);
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    let block = fixture
        .service
        .admit_native_filesystem_hooks(r#"{"pane_id":"%1","action_type":"shell_command"}"#)
        .unwrap()
        .unwrap();
    assert_eq!(block.hook_id, "filesystem-guard");
    assert!(
        fixture
            .service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
}

/// A blocking failure policy remains a required protection even without the
/// required bit. Conversely, an excluded semantic matcher must not deny an
/// unrelated patch or enqueue its configured executable.
#[test]
fn filesystem_hook_admission_preserves_blocking_policy_and_matchers() {
    use crate::integrations::hooks::{HookMatcherGroup, HookMatcherOperator, HookMatcherPredicate};
    for matches in [true, false] {
        let mut fixture = Fixture::new();
        install_guard(&mut fixture, false, false);
        let mut definition = fixture.service.integration.hook_definitions()[0].clone();
        definition.on_failure = Some(HookOnFailure::Block);
        definition.matcher_groups = vec![HookMatcherGroup {
            predicates: vec![HookMatcherPredicate {
                path: "semantic_action_type".into(),
                operator: HookMatcherOperator::Equals(
                    if matches { "apply_patch" } else { "read_file" }.into(),
                ),
            }],
        }];
        fixture
            .service
            .integration
            .replace_hook_definitions(vec![definition]);
        let attempt = fixture.queue();
        let result = fixture.service.claim_native_filesystem_action(&attempt);
        if matches {
            assert!(result.is_err());
            fixture
                .service
                .take_unissued_native_filesystem_owner(&attempt)
                .unwrap();
        } else {
            let dispatch = result.unwrap().unwrap();
            dispatch.fence.cancel();
        }
        assert!(
            fixture
                .service
                .drain_program_hook_transition()
                .side_effects
                .is_empty()
        );
        assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
        assert!(!fixture.root.join("file").exists());
    }
}
