//! Actual auxiliary claims/native holds through the actor ordering port. This
//! qualifies reconsideration custody, not physical HTTP or fresh-request fairness.

use crate::runtime::{RuntimeBudgetAttemptAdmission, RuntimeBudgetAuxiliarySource};
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use std::sync::Arc;

#[tokio::test]
async fn runtime_budget_reconsideration_orders_families_and_survives_epoch_revocation() {
    Box::pin(qualify(false)).await;
}

#[tokio::test]
async fn runtime_budget_reconsideration_cancelled_writer_retains_selected_window() {
    Box::pin(qualify(true)).await;
}

async fn qualify(cancelled: bool) {
    let (mut scene, primary, conversation) = super::auxiliary::setup();
    scene
        .service
        .schedule_runtime_agent_session_title(&conversation, Some("Original shared ordering"))
        .unwrap();
    let dispatch = scene
        .service
        .pending_agent_session_title_dispatch_id(&conversation)
        .unwrap()
        .to_owned();
    super::auxiliary::prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Title {
            conversation: conversation.clone(),
            dispatch: dispatch.clone(),
        },
    )
    .await
    .unwrap();
    let title = scene
        .service
        .claim_agent_session_title_dispatch(&conversation, &dispatch)
        .unwrap()
        .unwrap();
    scene
        .service
        .execute_agent_shell_command(&primary, "/remember Preserve original ordering.")
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id("%1")
        .unwrap()
        .to_owned();
    super::auxiliary::prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Remember {
            pane: "%1".into(),
            observation: observation.clone(),
        },
    )
    .await
    .unwrap();
    let remember = scene
        .service
        .claim_agent_remember_task("%1", &observation)
        .unwrap()
        .unwrap();
    assert_eq!(
        title.budget_owner.as_ref().unwrap().member.root,
        remember.budget_owner.as_ref().unwrap().member.root
    );
    let title_work = title
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Title,
        )
        .unwrap();
    let title_admission = title_work.admission_owner();
    let memory_admission = remember
        .budget_owner
        .as_ref()
        .unwrap()
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
            BudgetAttemptPurpose::Memory,
        )
        .unwrap()
        .admission_owner();
    assert!(matches!(
        title_admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    assert!(matches!(
        memory_admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let title_hold = title_admission.capture_held().await.unwrap();
    let memory_hold = memory_admission.capture_held().await.unwrap();
    let independent = if cancelled {
        let pane = scene
            .service
            .session_mut_for_tests()
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
            .unwrap();
        let conversation = scene
            .service
            .agent_shell_store_mut()
            .enter_or_resume(pane.as_str())
            .unwrap()
            .session_id
            .clone();
        scene
            .service
            .schedule_runtime_agent_session_title(
                &conversation,
                Some("Independent writer progress"),
            )
            .unwrap();
        let dispatch = scene
            .service
            .pending_agent_session_title_dispatch_id(&conversation)
            .unwrap()
            .to_owned();
        super::auxiliary::prepare(
            &mut scene,
            &RuntimeBudgetAuxiliarySource::Title {
                conversation: conversation.clone(),
                dispatch: dispatch.clone(),
            },
        )
        .await
        .unwrap();
        let dispatch = scene
            .service
            .claim_agent_session_title_dispatch(&conversation, &dispatch)
            .unwrap()
            .unwrap();
        let admission = dispatch
            .budget_owner
            .as_ref()
            .unwrap()
            .prepare_attempt(
                mez_agent::ModelTokenUsageKey::new("openai", "gpt-test"),
                BudgetAttemptPurpose::Title,
            )
            .unwrap()
            .admission_owner();
        assert!(matches!(
            admission.claim().await.unwrap(),
            RuntimeBudgetAttemptAdmission::Held(_)
        ));
        let capture = admission.capture_held().await.unwrap();
        scene
            .service
            .execute_terminal_command(&primary, "select-pane -t %1")
            .unwrap();
        Some((dispatch, admission, capture))
    } else {
        None
    };
    let service = std::mem::replace(&mut scene.service, super::test_runtime_service());
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let title_future = Arc::new(());
        let memory_future = Arc::new(());
        handle
            .park_budget_auxiliary_claim(title_hold.clone(), Arc::downgrade(&title_future))
            .await
            .unwrap();
        handle
            .park_budget_auxiliary_claim(memory_hold.clone(), Arc::downgrade(&memory_future))
            .await
            .unwrap();
        let title_window = Arc::new(());
        let memory_window = Arc::new(());
        let first = handle
            .select_budget_reconsideration(
                title_hold.clone(),
                Arc::downgrade(&title_future),
                Arc::downgrade(&title_window),
            )
            .await
            .unwrap();
        let second = handle
            .select_budget_reconsideration(
                memory_hold.clone(),
                Arc::downgrade(&memory_future),
                Arc::downgrade(&memory_window),
            )
            .await
            .unwrap();
        let duplicate = handle
            .select_budget_reconsideration(
                title_hold.clone(),
                Arc::downgrade(&title_future),
                Arc::downgrade(&title_window),
            )
            .await
            .unwrap();
        // Native reconsideration invalidates the prior positive hold before its
        // writer settles. Keeping the exact window must still exclude memory.
        let normal_window = (!cancelled).then(|| title_window.clone());
        let (release, blocked) = std::sync::mpsc::channel();
        let writer = if cancelled {
            let owner = title_admission.clone();
            let execute = title_work.clone();
            let retained = title_window.clone();
            let (started, ready) = tokio::sync::oneshot::channel();
            let writer = tokio::spawn(async move {
                owner
                    .retry_held_with_for_test(Some(retained), move || {
                        let _ = started.send(());
                        blocked
                            .recv_timeout(std::time::Duration::from_secs(10))
                            .map_err(|_| {
                                crate::error::MezError::invalid_state("fixture writer gate closed")
                            })?;
                        execute.authorize()
                    })
                    .await
            });
            ready.await.unwrap();
            writer.abort();
            Some(writer.await.unwrap_err().is_cancelled())
        } else {
            None
        };
        drop(title_window);
        let held_again = cancelled
            || matches!(
                title_admission.retry_held().await.unwrap(),
                RuntimeBudgetAttemptAdmission::Held(_)
            );
        let during = handle
            .select_budget_reconsideration(
                memory_hold.clone(),
                Arc::downgrade(&memory_future),
                Arc::downgrade(&memory_window),
            )
            .await
            .unwrap();
        let independent_progress = if let Some((_, _, capture)) = &independent {
            let future = Arc::new(());
            let window = Arc::new(());
            handle
                .park_budget_auxiliary_claim(capture.clone(), Arc::downgrade(&future))
                .await
                .unwrap();
            handle
                .select_budget_reconsideration(
                    capture.clone(),
                    Arc::downgrade(&future),
                    Arc::downgrade(&window),
                )
                .await
                .unwrap()
        } else {
            true
        };
        if cancelled {
            release.send(()).unwrap();
            assert!(matches!(
                title_admission.claim().await.unwrap(),
                RuntimeBudgetAttemptAdmission::Held(_)
            ));
            assert_eq!(writer, Some(true));
        }
        drop(normal_window);
        let recovered = handle
            .select_budget_reconsideration(
                memory_hold.clone(),
                Arc::downgrade(&memory_future),
                Arc::downgrade(&memory_window),
            )
            .await
            .unwrap();
        drop(memory_window);
        let stale_window = Arc::new(());
        let stale = handle
            .select_budget_reconsideration(
                title_hold.clone(),
                Arc::downgrade(&title_future),
                Arc::downgrade(&stale_window),
            )
            .await
            .is_err();
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        let retired_window = Arc::new(());
        let retired = handle
            .select_budget_reconsideration(
                memory_hold.clone(),
                Arc::downgrade(&memory_future),
                Arc::downgrade(&retired_window),
            )
            .await
            .is_err();
        handle.shutdown().await.unwrap();
        (
            first,
            second,
            duplicate,
            held_again,
            during,
            recovered,
            stale,
            retired,
            independent_progress,
        )
    };
    let (results, exit) = tokio::join!(client, actor.run());
    scene.service = exit.service;
    drop((
        handle,
        title_hold,
        memory_hold,
        title_admission,
        memory_admission,
        title,
        remember,
        independent,
    ));
    drop(scene); // Exact native fixture/root cleanup before final ordering oracles.
    assert_eq!(
        results,
        (true, false, true, true, false, true, true, true, true)
    );
}
