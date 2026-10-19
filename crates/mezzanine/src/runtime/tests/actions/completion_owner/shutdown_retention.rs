//! Additive shutdown attribution and bounded multi-owner lexical close controls.
//!
//! Actor admissions/claims are real; all completed worker payloads are synthetic.
//! Model-only splitting below does not start a pane process. No transport,
//! provider, native, descendant, no-shell or rollback workload is exercised.
//! Lexical release controls are not successful shared-shutdown-group proofs.

use super::*;
use crate::runtime::agent::CompletionScope;
use crate::runtime::processes::native_cancellation::NativeActionCancellation;

/// Compares all diagnostic fields of generated typed errors, including flags.
/// Only fixture-owned sentinel data is formatted; neither value is published.
fn assert_error_exact(actual: &MezError, expected: &MezError) {
    assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
}

/// Prepares a second actual actor execution without claiming a worker. The
/// model-only split starts no process; all async work precedes observations.
async fn prepare_second_execution(
    fixture: &mut CompletionFixture,
) -> (AgentTurnRecord, Vec<AgentAction>) {
    let mut execution = fixture.retained();
    let pane = fixture
        .service
        .session
        .split_active_pane(&fixture.primary, SplitDirection::Vertical)
        .unwrap()
        .to_string();
    fixture
        .service
        .agent_shell_store_mut()
        .enter_or_resume(&pane)
        .unwrap();
    let started = fixture
        .service
        .start_agent_prompt_turn(&pane, "second scope owner")
        .unwrap();
    fixture
        .service
        .remove_pending_agent_provider_task(&started.turn_id);
    let turn = fixture
        .service
        .agent_turn_ledger()
        .turn(&started.turn_id)
        .unwrap()
        .clone();
    assert_eq!(turn.state, AgentTurnState::Running);
    execution.request = runtime_model_request_fixture_for_agent(&turn.turn_id, &turn.agent_id);
    let actions = execution
        .response
        .action_batch
        .as_ref()
        .unwrap()
        .actions
        .clone();
    execution.action_results = actions
        .iter()
        .map(|action| {
            ActionResult::running(&turn, action, vec!["real second admission".into()], None)
        })
        .collect();
    fixture
        .service
        .apply_agent_provider_completed_event(
            &AgentId::opaque(turn.agent_id.clone()).unwrap(),
            &turn.turn_id,
            execution,
        )
        .await
        .unwrap();
    (turn, actions)
}

/// Claims a genuinely independent second group in the SAME actor before any
/// observation is captured; no group identity or stopped capability is forged.
async fn admit_second_group(
    fixture: &mut CompletionFixture,
) -> (AgentTurnRecord, String, NativeActionCancellation) {
    let (turn, actions) = prepare_second_execution(fixture).await;
    let dispatch = fixture
        .service
        .claim_approved_external_action(&turn.turn_id, &actions[0].id)
        .unwrap()
        .expect("genuine second actor claim");
    let (_, group) = fixture
        .service
        .completion_source_group_for_tests(&turn.turn_id)
        .unwrap();
    (turn, group, dispatch.cancellation)
}

/// A consumed error BEFORE descendants/cancellation must remain attributed to
/// the exact original group for both original-store and frozen no-save policy.
/// This is the new regression: without the outer caller boundary the returned
/// cut has no group-owned required failure. No clear or post-clear loss is claimed.
#[tokio::test]
async fn runtime_completion_shutdown_early_error_retains_original_without_cleanup() {
    for attach_store in [true, false] {
        let mut fixture = CompletionFixture::fetch_with_save_policy(true, attach_store).await;
        let before = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        let error = MezError::conflict("shutdown before descendants")
            .mark_pane_backend_failure()
            .with_provider_raw_text("fixture-only diagnostic")
            .with_provider_failure_json("{\"status_code\":409}");
        fixture
            .service
            .fail_completion_shutdown_before_descendants_for_tests(error.clone());
        let actual = fixture
            .service
            .fail_agent_turns_for_pane_shutdown(
                &[fixture.turn.pane_id.clone()],
                "fixture requested shutdown",
            )
            .unwrap_err();
        assert_error_exact(&actual, &error);
        assert!(
            fixture
                .service
                .take_completion_shutdown_fault_for_tests()
                .is_none()
        );
        let after = fixture
            .service
            .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
            .unwrap();
        assert_eq!(after.0, before.0);
        assert_eq!(after.1, before.1);
        assert_error_exact(after.2.as_ref().unwrap(), &error);
        // Actual live maps, not a failed-backing fallback, still own both values.
        assert_eq!(fixture.retained(), before.0);
        assert_eq!(
            fixture
                .service
                .agent_turn_contexts()
                .get(&fixture.turn.turn_id),
            Some(&before.1)
        );
        assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
        assert!(fixture.service.approved_external_action_attempt_is_current(
            &fixture.turn.turn_id,
            &fixture.action.id,
            &fixture.attempt,
        ));
        assert!(!fixture.service.pane_is_closing(&fixture.turn.pane_id));
        assert!(!fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::PaneRemoval,
        ));
        let failures = fixture
            .service
            .completion_required_failures_for_tests(&fixture.turn.turn_id);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, "pane_shutdown");
        assert_error_exact(&failures[0].1, &error);
    }
}

/// Actual shutdown of an unfinished group refuses ordinary positive retirement.
/// Cancellation and existing Finish error cleanup happen BEFORE this refusal
/// returns: the oracle is exact original failed custody and independent outer
/// attribution, NOT no cancellation or invented successful late-clear behavior.
#[tokio::test]
async fn runtime_completion_shutdown_refusal_keeps_original_and_outer_error() {
    let mut fixture = CompletionFixture::fetch().await;
    let error = fixture
        .service
        .fail_agent_turns_for_pane_shutdown(
            &[fixture.turn.pane_id.clone()],
            "fixture unfinished group",
        )
        .unwrap_err();
    assert!(
        error
            .message()
            .contains("has not transferred its retirement evidence"),
        "{error}"
    );
    assert!(fixture.cancellation.flag().load(Ordering::SeqCst));
    let original = fixture
        .service
        .original_completion_source_for_tests(&fixture.turn.turn_id, &fixture.group)
        .expect("actual original failed backing remains owned");
    assert_error_exact(original.2.as_ref().unwrap(), &error);
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
    let outer = failures
        .iter()
        .find(|(stage, _)| *stage == "pane_shutdown")
        .unwrap();
    assert_error_exact(&outer.1, &error);
    assert!(
        !fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::PaneRemoval,
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

/// A genuine prompt with no admitted worker group can shut down normally. The
/// empty observation set must not create a group, failure or archive obligation.
/// This qualifies None-at-entry only, not healthy retained-group shutdown.
#[test]
fn runtime_completion_shutdown_without_group_remains_ordinary() {
    let root = CompletionRoot(temp_root("shutdown-no-completion-group"));
    let mut service = RuntimeServiceFixture::new()
        .control_socket(root.0.join("control.sock"))
        .build();
    service
        .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let turn = service
        .start_agent_prompt_turn("%1", "no worker admission")
        .unwrap();
    service.remove_pending_agent_provider_task(&turn.turn_id);
    assert!(
        service
            .completion_source_group_for_tests(&turn.turn_id)
            .is_none()
    );
    assert_eq!(
        service
            .fail_agent_turns_for_pane_shutdown(&["%1".into()], "fixture no group")
            .unwrap(),
        1
    );
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
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
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
}

/// Actual final synthetic callbacks discharge terminal consumers and clear live
/// canonical maps while an outer set pins the group. Own close then releases
/// it. Original candidate and RuntimeOnly policy are asserted separately; this
/// is lexical/callback release readiness, not the shared shutdown happy path.
#[tokio::test]
async fn runtime_completion_scope_set_releases_after_actual_terminal_transfer() {
    for attach_store in [true, false] {
        let mut fixture = CompletionFixture::fetch_with_save_policy(true, attach_store).await;
        let panes = [fixture.turn.pane_id.as_str()].into_iter().collect();
        let observations = fixture
            .service
            .begin_completion_scopes_for_panes(&panes, CompletionScope::PaneRemoval);
        let sibling = fixture.retained().response.action_batch.unwrap().actions[1].clone();
        let outcome = fixture.outcome();
        assert!(
            fixture
                .service
                .complete_approved_external_action(outcome)
                .unwrap()
        );
        let dispatch = fixture
            .service
            .claim_approved_external_action(&fixture.turn.turn_id, &sibling.id)
            .unwrap()
            .unwrap();
        assert!(
            fixture
                .service
                .complete_approved_external_action(
                    crate::runtime::RuntimeApprovedExternalActionOutcome {
                        turn_id: fixture.turn.turn_id.clone(),
                        action_id: sibling.id.clone(),
                        attempt: dispatch.attempt,
                        result: Ok(ActionResult::succeeded(
                            &fixture.turn,
                            &sibling,
                            vec!["terminal scope sibling".into()],
                            None
                        )),
                        mcp_transport: None,
                    }
                )
                .unwrap()
        );
        assert_eq!(
            fixture
                .service
                .agent_turn_ledger()
                .turn(&fixture.turn.turn_id)
                .unwrap()
                .state,
            AgentTurnState::Completed
        );
        assert!(
            !fixture
                .service
                .agent_turn_executions()
                .contains_key(&fixture.turn.turn_id)
        );
        assert!(
            !fixture
                .service
                .agent_turn_contexts()
                .contains_key(&fixture.turn.turn_id)
        );
        assert!(fixture.service.completion_scope_is_active_for_tests(
            &fixture.turn.turn_id,
            CompletionScope::PaneRemoval
        ));
        let (policy, group) = fixture
            .service
            .completion_source_group_for_tests(&fixture.turn.turn_id)
            .unwrap();
        assert_eq!(group, fixture.group);
        assert_eq!(
            policy,
            if attach_store {
                "original"
            } else {
                "runtime_only"
            }
        );
        assert_eq!(
            fixture
                .service
                .retained_completion_snapshot_for_tests(&fixture.turn.turn_id)
                .unwrap()
                .is_some(),
            attach_store
        );
        assert!(
            fixture
                .service
                .completion_required_failures_for_tests(&fixture.turn.turn_id)
                .is_empty()
        );
        assert_eq!(
            fixture
                .service
                .end_completion_scopes(observations, "lexical_terminal_close", Ok(47))
                .unwrap(),
            47
        );
        assert!(
            fixture
                .service
                .completion_source_group_for_tests(&fixture.turn.turn_id)
                .is_none()
        );
    }
}

/// Two actual groups in one runtime keep independent existing primaries. An
/// operation failure remains aggregate primary and is separately retained as
/// secondary in both groups; closing the first cannot short-circuit the second.
#[tokio::test]
async fn runtime_completion_scope_set_operation_error_is_not_cross_attributed() {
    let mut fixture = CompletionFixture::fetch().await;
    let (second, second_group, cancellation) = admit_second_group(&mut fixture).await;
    let panes = [fixture.turn.pane_id.as_str(), second.pane_id.as_str()]
        .into_iter()
        .collect();
    let observations = fixture
        .service
        .begin_completion_scopes_for_panes(&panes, CompletionScope::PaneRemoval);
    let first = MezError::conflict("first original primary");
    let next = MezError::forbidden("second original primary");
    for (turn, error) in [(&fixture.turn, &first), (&second, &next)] {
        let nested = fixture
            .service
            .begin_completion_scope(&turn.turn_id, CompletionScope::Callback);
        let actual = fixture
            .service
            .end_completion_scope::<()>(nested, "own_primary", false, Err(error.clone()))
            .unwrap_err();
        assert_error_exact(&actual, error);
    }
    let operation =
        MezError::invalid_state("original shared operation error").mark_pane_backend_failure();
    let actual = fixture
        .service
        .end_completion_scopes::<()>(observations, "shared_close", Err(operation.clone()))
        .unwrap_err();
    assert_error_exact(&actual, &operation);
    for (turn, group, primary) in [
        (&fixture.turn, &fixture.group, &first),
        (&second, &second_group, &next),
    ] {
        let failures = fixture
            .service
            .completion_required_failures_for_tests(&turn.turn_id);
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].0, "own_primary");
        assert_error_exact(&failures[0].1, primary);
        assert_eq!(failures[1].0, "shared_close");
        assert_error_exact(&failures[1].1, &operation);
        assert_error_exact(
            fixture
                .service
                .original_completion_source_for_tests(&turn.turn_id, group)
                .unwrap()
                .2
                .as_ref()
                .unwrap(),
            primary,
        );
        assert!(
            !fixture
                .service
                .completion_scope_is_active_for_tests(&turn.turn_id, CompletionScope::PaneRemoval)
        );
    }
    assert!(!fixture.cancellation.flag().load(Ordering::SeqCst));
    assert!(!cancellation.flag().load(Ordering::SeqCst));
}

/// A real foreign observation precedes two exact same-runtime owners. Every
/// exact owner still closes. On successful operation an existing exact required
/// failure outranks mismatch, while without one the stale close is returned.
/// A same-turn-label current failure must not be read through the foreign
/// observation ahead of the next exact owner. All actual outer pins close.
#[tokio::test]
async fn runtime_completion_scope_set_closes_after_mismatch_and_prioritizes_exact_failure() {
    for required in [false, true] {
        let mut foreign = CompletionFixture::fetch().await;
        let mut fixture = CompletionFixture::fetch().await;
        let (second, _, _) = admit_second_group(&mut fixture).await;
        let foreign_outer = foreign
            .service
            .begin_completion_scope(&foreign.turn.turn_id, CompletionScope::PaneRemoval);
        let foreign_observation = foreign
            .service
            .begin_completion_scope(&foreign.turn.turn_id, CompletionScope::Stop);
        let one = fixture
            .service
            .begin_completion_scope(&fixture.turn.turn_id, CompletionScope::PaneRemoval);
        let two = fixture
            .service
            .begin_completion_scope(&second.turn_id, CompletionScope::PaneRemoval);
        let expected = MezError::forbidden("exact second required failure");
        if required {
            let nested = fixture
                .service
                .begin_completion_scope(&fixture.turn.turn_id, CompletionScope::Callback);
            fixture
                .service
                .end_completion_scope::<()>(
                    nested,
                    "other_exact_required",
                    false,
                    Err(MezError::conflict("same label is not the foreign original")),
                )
                .unwrap_err();
            let nested = fixture
                .service
                .begin_completion_scope(&second.turn_id, CompletionScope::Finish);
            fixture
                .service
                .end_completion_scope::<()>(nested, "exact_required", false, Err(expected.clone()))
                .unwrap_err();
        }
        let observations = RuntimeSessionService::completion_scope_set_for_tests(vec![
            foreign_observation,
            two,
            one,
        ]);
        let actual = fixture
            .service
            .end_completion_scopes(observations, "mismatch_close", Ok(53))
            .unwrap_err();
        if required {
            assert_error_exact(&actual, &expected);
        } else {
            assert!(actual.message().contains("scope owner changed"));
        }
        for turn in [&fixture.turn, &second] {
            assert!(
                !fixture.service.completion_scope_is_active_for_tests(
                    &turn.turn_id,
                    CompletionScope::PaneRemoval
                )
            );
        }
        assert_eq!(
            fixture
                .service
                .completion_required_failures_for_tests(&fixture.turn.turn_id)
                .len(),
            usize::from(required)
        );
        let failures = fixture
            .service
            .completion_required_failures_for_tests(&second.turn_id);
        assert_eq!(failures.len(), usize::from(required));
        assert!(
            foreign
                .service
                .completion_required_failures_for_tests(&foreign.turn.turn_id)
                .is_empty()
        );
        // Foreign nested close cannot clear its original runtime's outer pin.
        assert!(foreign.service.completion_scope_is_active_for_tests(
            &foreign.turn.turn_id,
            CompletionScope::PaneRemoval
        ));
        foreign
            .service
            .end_completion_scope(foreign_outer, "foreign_outer_close", false, Ok(()))
            .unwrap();
    }
}

/// An empty set captured before a second real group exists never adopts it at
/// close. The late group's independently installed scope, claims and canonical
/// context remain intact; an operation error is returned but not misattributed.
#[tokio::test]
async fn runtime_completion_scope_set_does_not_adopt_later_admission() {
    let mut fixture = CompletionFixture::fetch().await;
    let (second, actions) = prepare_second_execution(&mut fixture).await;
    assert!(
        fixture
            .service
            .completion_source_group_for_tests(&second.turn_id)
            .is_none()
    );
    let empty = fixture.service.begin_completion_scopes_for_panes(
        &[second.pane_id.as_str()].into_iter().collect(),
        CompletionScope::PaneRemoval,
    );
    let dispatch = fixture
        .service
        .claim_approved_external_action(&second.turn_id, &actions[0].id)
        .unwrap()
        .unwrap();
    let (_, group) = fixture
        .service
        .completion_source_group_for_tests(&second.turn_id)
        .unwrap();
    let own = fixture
        .service
        .begin_completion_scope(&second.turn_id, CompletionScope::Finish);
    let before = fixture
        .service
        .original_completion_source_for_tests(&second.turn_id, &group)
        .unwrap();
    let error = MezError::conflict("empty original caller");
    assert_error_exact(
        &fixture
            .service
            .end_completion_scopes::<()>(empty, "no_late_adoption", Err(error.clone()))
            .unwrap_err(),
        &error,
    );
    assert!(
        fixture
            .service
            .completion_scope_is_active_for_tests(&second.turn_id, CompletionScope::Finish)
    );
    assert!(
        fixture
            .service
            .completion_required_failures_for_tests(&second.turn_id)
            .is_empty()
    );
    let after = fixture
        .service
        .original_completion_source_for_tests(&second.turn_id, &group)
        .unwrap();
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1);
    assert!(!dispatch.cancellation.flag().load(Ordering::SeqCst));
    fixture
        .service
        .end_completion_scope(own, "late_own_close", false, Ok(()))
        .unwrap();
}
