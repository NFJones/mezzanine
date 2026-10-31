//! Real confirmed filesystem facts through immutable ordered final publication.
//!
//! Tests exercise source retention, partial effects, actual install barriers,
//! duplicate callbacks and current-conversation fencing without worker replay.
//! Publication acceptance never supplies archive or producer release authority.

use super::canonical::{execute_and_retain, result_events};
use super::*;

/// Returns only this action's exact installed RESULT source acknowledgements.
fn results(fixture: &Fixture) -> Vec<String> {
    let expected = fixture
        .execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .iter()
        .position(|action| action.id == fixture.action.id)
        .unwrap();
    fixture
        .service
        .presentation
        .agent_published_provider_results
        .iter()
        .filter(|((_, turn, _, _, ordinal), _)| {
            turn == &fixture.turn.turn_id && *ordinal == expected
        })
        .map(|(_, source)| source.clone())
        .collect()
}

/// A real native effect installs its confirmed diff, semantic outcome and header
/// once. Canonical result/context and full modification counts stay unchanged;
/// publication grants neither another worker nor provider continuation.
#[test]
fn native_filesystem_publication_installs_confirmed_diff_once() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert!(
        !fixture
            .service
            .publish_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert!(results(&fixture).is_empty());
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let canonical = fixture.service.agent_turn_executions()[&fixture.turn.turn_id].clone();
    let installed = results(&fixture);
    assert_eq!(installed.len(), 1);
    assert!(installed[0].contains("+native data"));
    let text = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(text.contains("native patch: confirmed"));
    assert!(!text.contains("/bin/sh"));
    assert!(
        !fixture
            .service
            .publish_native_filesystem_completion(&attempt)
            .unwrap()
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(results(&fixture), installed);
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id],
        canonical
    );
    assert_eq!(result_events(&fixture), 1);
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

/// A failed pre-install boundary retains exact accepted component entries and
/// original bytes. The common publication pump installs them after recovery
/// without rerunning projection, native I/O, audit or earlier acknowledged rows.
#[test]
fn native_filesystem_publication_retries_only_immutable_display_sources() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    assert!(
        fixture
            .service
            .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
    );
    let retained = fixture
        .service
        .presentation
        .agent_queued_provider_results
        .clone();
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(
        fixture.service.presentation.agent_queued_provider_results,
        retained
    );
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(false);
    assert!(fixture.service.retry_completion_publications() > 0);
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
    );
    assert_eq!(results(&fixture).len(), 1);
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(fixture.service.retry_completion_publications(), 0);
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

/// A genuine partial move publishes its positive destination diff and explicit
/// partial certainty, while preserving the source and failed canonical result.
/// Display does not invent whole-move success or hide the confirmed endpoint.
#[test]
fn native_filesystem_publication_keeps_partial_effect_diff_and_failure_summary() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n*** Move to: {}\n@@\n-old\n+new\n*** End Patch",
            root.join("a").display(),
            root.join("z").display()
        )
    });
    fs::write(fixture.root.join("a"), b"old\n").unwrap();
    let (attempt, _) = execute_and_retain(&mut fixture, true);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert_eq!(results(&fixture).len(), 1);
    assert!(results(&fixture)[0].contains("+new"));
    let text = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(text.contains("native patch: partial"));
    assert_eq!(
        fixture.service.agent_turn_executions()[&fixture.turn.turn_id].action_results[0].status,
        mez_agent::ActionStatus::Failed
    );
    assert_eq!(fs::read(fixture.root.join("a")).unwrap(), b"old\n");
    assert_eq!(fs::read(fixture.root.join("z")).unwrap(), b"new\n");
}

/// A new conversation cannot adopt an older display source or turn it into
/// actionable retry work. Original canonical/diff evidence remains held and no
/// status/result is painted into the replacement screen or marked installed.
#[test]
fn native_filesystem_publication_stale_conversation_retains_source_without_paint() {
    let mut fixture = Fixture::new();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .session_id = "later-publication-cid".into();
    let screen = fixture.service.agent_pane_screen("%1").unwrap().clone();
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(results(&fixture).is_empty());
    assert_eq!(fixture.service.agent_pane_screen("%1").unwrap(), &screen);
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
            .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&attempt)
            .unwrap()
    ));
    assert_eq!(result_events(&fixture), 1);
}

/// A genuine update no-op has no confirmed endpoint or diff. It publishes the
/// native zero-change outcome without a phantom RESULT row or mutation replay.
#[test]
fn native_filesystem_publication_noop_has_no_phantom_diff_source() {
    let mut fixture = Fixture::with_patch(false, |root| {
        format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n same\n*** End Patch",
            root.join("file").display()
        )
    });
    fs::write(fixture.root.join("file"), b"same\n").unwrap();
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    assert_eq!(raw.result.certainty, NativeEffectCertainty::Confirmed);
    assert!(raw.receipts.is_empty());
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(results(&fixture).is_empty());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_accepted"
    );
    assert!(
        fixture
            .service
            .agent_pane_screen("%1")
            .unwrap()
            .visible_lines()
            .join("\n")
            .contains("0 confirmed file endpoint")
    );
    assert_eq!(fs::read(fixture.root.join("file")).unwrap(), b"same\n");
}

/// A real native effect cannot paint past an earlier running ordinal. The prior
/// list-agents result is an inert presentation fixture, not a query invocation.
/// Once its immutable source installs, the ordinary pump publishes native facts
/// without a second file mutation or provider continuation across the group.
#[test]
fn native_filesystem_publication_waits_for_original_predecessor_prefix() {
    let mut fixture = Fixture::with_actions(false, |root| {
        vec![
            mez_agent::AgentAction {
                id: "prefix".into(),
                payload: mez_agent::AgentActionPayload::ListAgents {
                    agent_type: None,
                    scope: None,
                },
            },
            mez_agent::AgentAction {
                id: "native-file".into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+native data\n*** End Patch",
                        root.join("file").display()
                    ),
                    strip: None,
                },
            },
        ]
    });
    let (attempt, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    assert!(results(&fixture).is_empty());
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
            .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
    );
    let mut execution = fixture.service.agent_turn_executions()[&fixture.turn.turn_id].clone();
    let prefix = execution.response.action_batch.as_ref().unwrap().actions[0].clone();
    execution.action_results[0] = mez_agent::ActionResult::succeeded(
        &fixture.turn,
        &prefix,
        vec!["PREFIX RESULT".into()],
        None,
    );
    fixture
        .service
        .agent_turn_executions_mut()
        .insert(fixture.turn.turn_id.clone(), execution.clone());
    fixture
        .service
        .queue_ordered_provider_header("%1", &execution, &prefix)
        .unwrap();
    fixture
        .service
        .queue_ordered_provider_result("%1", &execution, &prefix, &execution.action_results[0])
        .unwrap();
    assert!(fixture.service.retry_completion_publications() > 0);
    assert_eq!(results(&fixture).len(), 1);
    let screen = fixture
        .service
        .agent_pane_screen("%1")
        .unwrap()
        .visible_lines()
        .join("\n");
    assert!(screen.find("PREFIX RESULT").unwrap() < screen.find("+native data").unwrap());
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
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}

/// Holding the exact accepted outcome entry lets the confirmed diff install
/// first. Retrying that suffix cannot reinstall the acknowledged diff/header or
/// replace the original outcome occurrence with another acceptance timestamp.
#[test]
fn native_filesystem_publication_outcome_hold_preserves_installed_prefix() {
    let mut fixture = Fixture::new();
    let (attempt, _) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(true);
    fixture
        .service
        .project_native_filesystem_completion(&attempt)
        .unwrap();
    let outcome = fixture
        .service
        .presentation
        .agent_queued_provider_outcomes
        .values()
        .next()
        .unwrap()[0]
        .clone();
    fixture
        .service
        .hold_agent_presentation_entry_for_tests("%1", &outcome.2.id, true);
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(results(&fixture).len(), 1);
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&attempt)
            .unwrap(),
        "filesystem_publication_held"
    );
    let prefix = results(&fixture);
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(results(&fixture), prefix);
    assert_eq!(
        fixture
            .service
            .presentation
            .agent_queued_provider_outcomes
            .values()
            .next()
            .unwrap()[0]
            .2
            .id,
        outcome.2.id
    );
    fixture
        .service
        .hold_agent_presentation_entry_for_tests("%1", &outcome.2.id, false);
    assert!(fixture.service.retry_completion_publications() > 0);
    assert_eq!(results(&fixture), prefix);
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert_eq!(result_events(&fixture), 1);
    assert_eq!(
        fs::read(fixture.root.join("file")).unwrap(),
        b"native data\n"
    );
}

/// An actual earlier queued filesystem owner still owns required gates even if
/// its intent header is already visible. A genuinely committed later patch
/// cannot publish past it or turn header acknowledgement into settled authority.
#[test]
fn native_filesystem_publication_visible_header_does_not_acknowledge_earlier_filesystem_owner() {
    let mut fixture = Fixture::with_actions(false, |root| {
        ["first", "second"]
            .into_iter()
            .map(|name| mez_agent::AgentAction {
                id: name.into(),
                payload: mez_agent::AgentActionPayload::ApplyPatch {
                    patch: format!(
                        "*** Begin Patch\n*** Add File: {}\n+native data\n*** End Patch",
                        root.join(name).display()
                    ),
                    strip: None,
                },
            })
            .collect()
    });
    let first = fixture.queue();
    fixture
        .service
        .queue_ordered_provider_header("%1", &fixture.execution, &fixture.action)
        .unwrap();
    fixture
        .service
        .flush_ordered_provider_headers("%1", &fixture.execution)
        .unwrap();
    fixture.action = fixture
        .execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions[1]
        .clone();
    let (second, raw) = execute_and_retain(&mut fixture, false);
    fixture
        .service
        .project_native_filesystem_completion(&second)
        .unwrap();
    assert!(!fixture.root.join("first").exists());
    assert_eq!(
        fs::read(fixture.root.join("second")).unwrap(),
        b"native data\n"
    );
    assert!(results(&fixture).is_empty());
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&first)
            .unwrap(),
        "filesystem_queued"
    );
    assert_eq!(
        fixture
            .service
            .filesystem_completion_phase_for_tests(&second)
            .unwrap(),
        "filesystem_publication_held"
    );
    assert!(
        !fixture
            .service
            .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
    );
    assert_eq!(fixture.service.retry_completion_publications(), 0);
    assert!(Arc::ptr_eq(
        &raw,
        &fixture
            .service
            .retained_native_filesystem_outcome(&second)
            .unwrap()
    ));
    assert!(
        !fixture
            .service
            .completion_provider_continuation_ready(&fixture.turn.turn_id)
    );
}
