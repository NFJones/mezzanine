//! Original filesystem completion-hook custody after real descriptor effects.
//!
//! Frozen definitions, typed outcome matching, changed pane presentation and
//! partial diagnostic failures exercise the actual actor projection boundary.
//! No test treats hook acceptance as audit, archive or producer release.

use super::canonical::{execute_and_retain, result_events};
use super::*;
use crate::integrations::hooks::{
    HookDefinition, HookInvocation, HookMatcherGroup, HookMatcherOperator, HookMatcherPredicate,
    HookOnFailure,
};
use crate::runtime::EventLog;

/// Installs original post-action definitions and a bounded observable event log.
/// Program and focused-shell handlers are both incompatible, even if required.
fn install_completed_hooks(fixture: &mut Fixture, oversized_second: bool, certainty: &str) {
    fixture.service.use_hook_effect_adapter();
    fixture
        .service
        .control
        .replace_event_log(Some(EventLog::new(100, 8192).unwrap()));
    let definitions = [false, true]
        .into_iter()
        .map(|focused| HookDefinition {
            id: if focused && oversized_second {
                "long".repeat(300)
            } else if focused {
                "focused-post".into()
            } else {
                "program-post".into()
            },
            event: HookEvent::PostShellCommand,
            invocation: if focused {
                HookInvocation::FocusedShell {
                    command: "printf forbidden-post".into(),
                }
            } else {
                HookInvocation::Program {
                    command: "/missing/forbidden-post".into(),
                    args: Vec::new(),
                }
            },
            enabled: true,
            required: true,
            agent_hook: false,
            matcher_groups: vec![HookMatcherGroup {
                predicates: vec![
                    HookMatcherPredicate {
                        path: "semantic_action_type".into(),
                        operator: HookMatcherOperator::Equals("apply_patch".into()),
                    },
                    HookMatcherPredicate {
                        path: "effect_certainty".into(),
                        operator: HookMatcherOperator::Equals(certainty.into()),
                    },
                    HookMatcherPredicate {
                        path: "exit_code".into(),
                        operator: HookMatcherOperator::Exists(false),
                    },
                    HookMatcherPredicate {
                        path: "command".into(),
                        operator: HookMatcherOperator::Exists(false),
                    },
                ],
            }],
            timeout_ms: None,
            on_failure: Some(HookOnFailure::Block),
        })
        .collect();
    fixture
        .service
        .integration
        .replace_hook_definitions(definitions);
}

/// Reads actual emitted incompatibility events, excluding unrelated lifecycle
/// bookkeeping; no event count is inferred from private phase labels.
fn hook_events(fixture: &Fixture) -> Vec<crate::protocol::event::VisibleEvent> {
    fixture
        .service
        .control
        .event_log()
        .unwrap()
        .replay_for(&EventAudience::AllPrimaries)
        .into_iter()
        .filter(|event| event.kind == EventKind::HookFailed)
        .collect()
}

/// Original required completion handlers are diagnosed exactly once after real
/// committed effects, despite later config removal and pane-mode/CID changes.
/// They cannot queue processes, paint a replacement conversation, replay facts,
/// release raw custody or satisfy the remaining provider continuation gate.
#[test]
fn native_filesystem_completed_hooks_keep_original_nonexecutable_ownership() {
    let mut fixture = Fixture::new();
    install_completed_hooks(&mut fixture, false, "confirmed");
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .integration
        .replace_hook_definitions(Vec::new());
    fixture
        .service
        .set_agent_shell_mode_override("%1", Some(crate::runtime::config::ShellMode::Pane));
    fixture
        .service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .session_id = "replacement-cid".into();
    let screen = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .visible_lines();
    assert!(
        fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    let events = hook_events(&fixture);
    assert_eq!(events.len(), 2);
    assert!(events[0].payload.contains("program-post"));
    assert!(events[1].payload.contains("focused-post"));
    assert_eq!(
        fixture
            .service
            .agent_pane_screen("%1")
            .unwrap()
            .visible_lines(),
        screen
    );
    assert!(
        !fixture
            .service
            .project_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(hook_events(&fixture), events);
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert!(
        fixture
            .service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Matching uses a genuine partial move outcome and does not invent whole-move
/// success, shell status or a rollback when required post handlers cannot run.
#[test]
fn native_filesystem_completed_hooks_match_actual_partial_effects() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("a").display(),
            root.join("z").display()
        )
    });
    fs::write(fixture.root.join("a"), b"old\n").unwrap();
    install_completed_hooks(&mut fixture, false, "partial");
    let (attempt, _) = execute_and_retain(&mut fixture, true);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert_eq!(hook_events(&fixture).len(), 2);
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Failed
    );
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"old\n");
    assert_eq!(fs::read(fixture.root.join("z")).unwrap(), b"new\n");
    assert!(
        fixture
            .service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
}

/// A real event-log failure after the first diagnostic leaves the completion
/// gate entered and unproven. Replacing the failed sink cannot replay the first
/// handler diagnostic, erase canonical effects or reset the first error.
#[test]
fn native_filesystem_completed_hook_diagnostic_failure_cannot_replay() {
    let mut fixture = Fixture::new();
    install_completed_hooks(&mut fixture, true, "confirmed");
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .control
        .replace_event_log(Some(EventLog::new(100, 512).unwrap()));
    let first = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert_eq!(hook_events(&fixture).len(), 1);
    assert!(hook_events(&fixture)[0].payload.contains("program-post"));
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_hooks_entered"
    );
    fixture
        .service
        .control
        .replace_event_log(Some(EventLog::new(100, 8192).unwrap()));
    let repeated = fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap_err();
    assert_eq!(first.message(), repeated.message());
    assert!(hook_events(&fixture).is_empty());
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Succeeded
    );
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// A completion matcher for another effect certainty is excluded without
/// emitting incompatibility diagnostics or falling back to executable hooks.
#[test]
fn native_filesystem_completed_hook_matchers_exclude_unrelated_outcomes() {
    let mut fixture = Fixture::new();
    install_completed_hooks(&mut fixture, false, "partial");
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(hook_events(&fixture).is_empty());
    assert!(
        fixture
            .service
            .drain_program_hook_transition()
            .side_effects
            .is_empty()
    );
    assert_eq!(fixture.service.focused_shell_hook_queue_len(), 0);
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
    );
}
