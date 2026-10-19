//! Explicit admitted pane-close and generic helper retention controls.
//!
//! Groups come from actual actor admission/claims; payloads are synthetic. Model
//! panes have their real default live flag without any physical process. These
//! tests do not execute provider/native/editor/descendant work or qualify the
//! omitted-target, no-shell, restore or failed-spawn caller families.

use super::*;
use crate::runtime::agent::CompletionScope;

/// Compares all generated diagnostic fields, including policy/backend flags.
/// Formatting is local to fixture-only errors and never produces runtime output.
fn assert_error_exact(actual: &MezError, expected: &MezError) {
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
}

/// Builds a canonical explicit target without invoking control/transport I/O.
fn close_params(pane_id: &str, force: bool) -> String {
    format!(r#"{{"target":{{"pane_id":"{pane_id}"}},"force":{force}}}"#)
}

/// Invalid target, detached caller and live/non-force rejection remain ordinary
/// unadmitted errors. None may install caller retention, consume the admitted
/// entry cut, cancel a real claim, change canonical values or latch a group error.
/// The live flag is the actual model default, not a fabricated native witness.
#[tokio::test]
async fn runtime_completion_pane_close_rejections_do_not_latch_original() {
    for rejection in ["invalid_target", "detached_primary", "live_without_force"] {
        let mut fixture = CompletionFixture::fetch().await;
        let original = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        let cut = MezError::invalid_state("unadmitted request must not reach caller cut");
        fixture
            .service
            .fail_pane_retention_entry_for_tests("pane_close", cut.clone());
        if rejection == "detached_primary" {
            fixture
                .service
                .detach_primary(&fixture.primary, Size::new(120, 32).unwrap())
                .unwrap();
        }
        if rejection == "live_without_force" {
            let (_, pane) =
                runtime_pane_by_id(&fixture.service.session, &fixture.turn.pane_id).unwrap();
            assert!(pane.live, "the actual model-created pane must be live");
        }
        let params = close_params(
            if rejection == "invalid_target" {
                "%not-present"
            } else {
                &fixture.turn.pane_id
            },
            rejection != "live_without_force",
        );
        let error = fixture
            .service
            .dispatch_runtime_pane_close(&fixture.primary, &params)
            .unwrap_err();
        assert_ne!(error.message(), cut.message());
        assert_eq!(
            error.kind(),
            if rejection == "invalid_target" {
                crate::error::MezErrorKind::NotFound
            } else {
                crate::error::MezErrorKind::Forbidden
            }
        );
        assert_error_exact(
            &fixture
                .service
                .take_pane_retention_fault_for_tests("pane_close")
                .unwrap(),
            &cut,
        );
        let retained = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        assert_eq!(retained.0, original.0);
        assert_eq!(retained.1, original.1);
        assert!(retained.2.is_none());
        assert!(
            fixture
                .service
                .completion_required_failures_for_tests(&fixture.turn.turn_id)
                .is_empty()
        );
        assert!(!fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::PaneRemoval
        ));
        assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
        assert!(fixture.service.approved_external_action_attempt_is_current(
            &fixture.turn.turn_id,
            &fixture.action.id,
            &fixture.attempt,
        ));
        assert!(
            fixture
                .service
                .find_pane_descriptor(&fixture.turn.pane_id)
                .is_some()
        );
    }
}

/// A force-admitted explicit target captures its real original group BEFORE
/// shared shutdown. A consumed early error is retained for Original/RuntimeOnly
/// without cancellation, backing moves, membership change or a native operation.
#[tokio::test]
async fn runtime_completion_pane_close_early_error_retains_admitted_original() {
    for attach_store in [true, false] {
        let mut fixture = CompletionFixture::fetch_with_save_policy(true, attach_store).await;
        let original = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        let error = MezError::conflict("admitted explicit close before shutdown")
            .mark_pane_backend_failure();
        fixture
            .service
            .fail_pane_retention_entry_for_tests("pane_close", error.clone());
        let params = close_params(&fixture.turn.pane_id, true);
        let actual = fixture
            .service
            .dispatch_runtime_pane_close(&fixture.primary, &params)
            .unwrap_err();
        assert_error_exact(&actual, &error);
        assert!(
            fixture
                .service
                .take_pane_retention_fault_for_tests("pane_close")
                .is_none()
        );
        let retained = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        assert_eq!(retained.0, original.0);
        assert_eq!(retained.1, original.1);
        assert_error_exact(retained.2.as_ref().unwrap(), &error);
        assert_eq!(fixture.retained(), original.0);
        assert_eq!(
            fixture
                .service
                .agent_turn_contexts()
                .get(&fixture.turn.turn_id),
            Some(&original.1)
        );
        let failures = fixture
            .service
            .completion_required_failures_for_tests(&fixture.turn.turn_id);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "pane_close");
        assert_error_exact(&failures[0].1, &error);
        assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
        assert!(fixture.service.approved_external_action_attempt_is_current(
            &fixture.turn.turn_id,
            &fixture.action.id,
            &fixture.attempt,
        ));
        assert!(!fixture.service.pane_is_closing(&fixture.turn.pane_id));
        assert!(
            fixture
                .service
                .find_pane_descriptor(&fixture.turn.pane_id)
                .is_some()
        );
        assert!(!fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::PaneRemoval
        ));
    }
}

/// A real admitted explicit close of an unfinished group reaches the existing
/// positive-retirement refusal. Existing Finish cancellation/failed backing is
/// expected; the new outer caller independently retains the same operation
/// error after shutdown close, without replaying any removal or suffix effect.
#[tokio::test]
async fn runtime_completion_pane_close_refusal_unwinds_all_original_stages() {
    let mut fixture = CompletionFixture::fetch().await;
    let params = close_params(&fixture.turn.pane_id, true);
    let error = fixture
        .service
        .dispatch_runtime_pane_close(&fixture.primary, &params)
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("has not transferred its retirement evidence"),
        "{error}"
    );
    assert!(fixture.cancellation.flag().load(Ordering::SeqCst));
    let retained = fixture
        .service
        .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
        .expect("real original failed custody");
    assert_error_exact(retained.2.as_ref().unwrap(), &error);
    assert!(
        !fixture
            .service
            .agent_turn_executions()
            .contains_key(&fixture.turn.turn_id)
    );
    let failures = fixture
        .service
        .completion_required_failures_for_tests(&fixture.turn.turn_id);
    assert_eq!(failures.first().unwrap().0, "turn_finalization");
    for stage in ["pane_shutdown", "pane_close"] {
        assert_error_exact(
            &failures.iter().find(|(name, _)| *name == stage).unwrap().1,
            &error,
        );
    }
    assert!(!failures.iter().any(|(stage, _)| *stage == "pane_cleanup"));
    assert!(
        !fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::PaneRemoval
        )
    );
    assert!(
        fixture
            .service
            .find_pane_descriptor(&fixture.turn.pane_id)
            .is_some()
    );
    assert_eq!(
        fixture
            .service
            .agent_turn_ledger()
            .turn(&fixture.turn.turn_id)
            .unwrap()
            .state,
        AgentTurnState::Running
    );
}

/// Generic helper entry is covered even when layout is present. A genuine
/// lexical outer Callback owns the group while the helper's nested error closes;
/// the outer then retains its own bounded secondary. This is a helper/API
/// placement control, NOT an executed restore or post-shutdown caller suffix.
#[tokio::test]
async fn runtime_completion_pane_helper_error_preserves_lexical_outer() {
    for attach_store in [true, false] {
        let mut fixture = CompletionFixture::fetch_with_save_policy(true, attach_store).await;
        let original = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        let observations = fixture.service.begin_completion_scopes_for_panes(
            &[fixture.turn.pane_id.as_str()].into_iter().collect(),
            CompletionScope::Callback,
        );
        let error = MezError::forbidden("helper before reconciliation and title")
            .mark_pane_backend_failure();
        fixture
            .service
            .fail_pane_retention_entry_for_tests("pane_cleanup", error.clone());
        let actual = fixture
            .service
            .cleanup_removed_pane_runtime_state(&fixture.turn.pane_id)
            .unwrap_err();
        assert_error_exact(&actual, &error);
        assert!(
            fixture
                .service
                .take_pane_retention_fault_for_tests("pane_cleanup")
                .is_none()
        );
        assert!(fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::Callback
        ));
        assert!(
            fixture
                .service
                .find_pane_descriptor(&fixture.turn.pane_id)
                .is_some()
        );
        assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
        let retained = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        assert_eq!(retained.0, original.0);
        assert_eq!(retained.1, original.1);
        assert_error_exact(retained.2.as_ref().unwrap(), &error);
        assert_eq!(fixture.retained(), original.0);
        let actual = fixture
            .service
            .end_completion_scopes::<()>(observations, "lexical_helper_outer", Err(error.clone()))
            .unwrap_err();
        assert_error_exact(&actual, &error);
        assert!(!fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::Callback
        ));
        let failures = fixture
            .service
            .completion_required_failures_for_tests(&fixture.turn.turn_id);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, "pane_cleanup");
        assert_eq!(failures[1].0, "lexical_helper_outer");
        for (_, failure) in failures {
            assert_error_exact(&failure, &error);
        }
    }
}

/// A real prompt without a worker group can reach the COMPLETE explicit close
/// suffix with no pane process. Optional helper fault occurs after actual model
/// membership removal, and its error does not replay that committed operation.
/// This qualifies no-group healthy/late-error behavior, not retained-group loss
/// or native process termination/editor cleanup/disk durability.
#[test]
fn runtime_completion_pane_close_no_group_settles_or_reports_committed_helper_error() {
    for fail_helper in [false, true] {
        let root = CompletionRoot(temp_root("pane-retention-no-group"));
        let mut service = RuntimeServiceFixture::new()
            .control_socket(root.0.join("control.sock"))
            .build();
        let primary = service
            .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let turn = service
            .start_agent_prompt_turn("%1", "ordinary explicit close")
            .unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        assert!(
            service
                .completion_source_group_for_tests(&turn.turn_id)
                .is_none()
        );
        let error = MezError::conflict("helper after actual no-group membership removal");
        if fail_helper {
            service.fail_pane_retention_entry_for_tests("pane_cleanup", error.clone());
        }
        let params = close_params("%1", true);
        let result = service.dispatch_runtime_pane_close(&primary, &params);
        if fail_helper {
            assert_error_exact(&result.unwrap_err(), &error);
            assert!(
                service
                    .take_pane_retention_fault_for_tests("pane_cleanup")
                    .is_none()
            );
        } else {
            let response: serde_json::Value = serde_json::from_str(&result.unwrap()).unwrap();
            assert_eq!(response["closed"], true);
            assert_eq!(response["terminated_panes"], 0);
            assert!(service.agent_shell_store().get("%1").is_none());
        }
        assert!(service.find_pane_descriptor("%1").is_none());
        assert_eq!(
            service
                .agent_turn_ledger()
                .turn(&turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Failed
        );
        assert!(
            service
                .completion_source_group_for_tests(&turn.turn_id)
                .is_none()
        );
        assert!(
            service
                .completion_required_failures_for_tests(&turn.turn_id)
                .is_empty()
        );
        let revision = service.session.mutation_revision();
        let second = service
            .dispatch_runtime_pane_close(&primary, &params)
            .unwrap_err();
        assert_eq!(second.kind(), crate::error::MezErrorKind::NotFound);
        assert_eq!(service.session.mutation_revision(), revision);
    }
}
