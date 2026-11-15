//! Actual title scheduling/claim/native hold: parked work retains original
//! custody but must not consume both optional-title execution slots.

use crate::runtime::{RuntimeBudgetAttemptAdmission, RuntimeBudgetAuxiliarySource};
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use std::sync::Arc;

#[tokio::test]
async fn runtime_budget_parked_titles_release_active_capacity() {
    let (mut scene, primary, first) = super::auxiliary::setup();
    let mut conversations = vec![first];
    for _ in 0..2 {
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
        conversations.push(conversation);
    }
    let waiter = Arc::new(());
    let mut owners = Vec::new();
    let mut holds = Vec::new();
    for conversation in &conversations[..2] {
        scene
            .service
            .schedule_runtime_agent_session_title(conversation, Some("Original title capacity"))
            .unwrap();
        let dispatch_id = scene
            .service
            .pending_agent_session_title_dispatch_id(conversation)
            .unwrap()
            .to_owned();
        super::auxiliary::prepare(
            &mut scene,
            &RuntimeBudgetAuxiliarySource::Title {
                conversation: conversation.clone(),
                dispatch: dispatch_id.clone(),
            },
        )
        .await
        .unwrap();
        let dispatch = scene
            .service
            .claim_agent_session_title_dispatch(conversation, &dispatch_id)
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
            .park_budget_title_claim(
                capture.clone(),
                Arc::downgrade(&waiter),
                crate::runtime::current_unix_millis(),
            )
            .unwrap();
        owners.push((dispatch, admission));
        holds.push(capture);
    }
    let admitted = scene
        .service
        .schedule_runtime_agent_session_title(&conversations[2], Some("Healthy independent title"));
    let first_resume = scene
        .service
        .resume_budget_title_claim(&holds[0], crate::runtime::current_unix_millis());
    let second_resume = scene
        .service
        .resume_budget_title_claim(&holds[1], crate::runtime::current_unix_millis());
    let original_pending = scene
        .service
        .pending_agent_session_title_dispatch_id(&conversations[2])
        .is_some();
    drop((owners, holds, waiter));
    drop(scene); // Exact fixture owner and root retire before RED oracles.
    assert!(
        admitted.is_ok(),
        "parked titles consumed active capacity: {admitted:?}"
    );
    assert!(original_pending);
    assert!(first_resume.unwrap());
    assert!(
        !second_resume.unwrap(),
        "resume exceeded the unchanged two-title cap"
    );
}

pub(super) async fn parked_title(
    scene: &mut super::Scene,
    conversation: &str,
    waiter: &Arc<()>,
) -> (
    crate::runtime::RuntimeAgentSessionTitleDispatch,
    crate::runtime::RuntimeBudgetAdmissionWork,
    crate::runtime::RuntimeBudgetHeldAttempt,
) {
    scene
        .service
        .schedule_runtime_agent_session_title(conversation, Some("Original ordered title"))
        .unwrap();
    let dispatch_id = scene
        .service
        .pending_agent_session_title_dispatch_id(conversation)
        .unwrap()
        .to_owned();
    super::auxiliary::prepare(
        scene,
        &RuntimeBudgetAuxiliarySource::Title {
            conversation: conversation.into(),
            dispatch: dispatch_id.clone(),
        },
    )
    .await
    .unwrap();
    let dispatch = scene
        .service
        .claim_agent_session_title_dispatch(conversation, &dispatch_id)
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
        .park_budget_title_claim(
            capture.clone(),
            Arc::downgrade(waiter),
            crate::runtime::current_unix_millis(),
        )
        .unwrap();
    (dispatch, admission, capture)
}

#[tokio::test]
async fn runtime_budget_title_ready_order_prevents_newcomer_and_poll_overtaking() {
    let (mut scene, primary, first) = super::auxiliary::setup();
    let mut conversations = vec![first];
    for _ in 0..3 {
        let pane = scene
            .service
            .session_mut_for_tests()
            .split_active_pane(&primary, mez_mux::layout::SplitDirection::Horizontal)
            .unwrap();
        conversations.push(
            scene
                .service
                .agent_shell_store_mut()
                .enter_or_resume(pane.as_str())
                .unwrap()
                .session_id
                .clone(),
        );
    }
    let waiter = Arc::new(());
    let first = parked_title(&mut scene, &conversations[0], &waiter).await;
    let second = parked_title(&mut scene, &conversations[1], &waiter).await;
    for conversation in &conversations[2..] {
        scene
            .service
            .schedule_runtime_agent_session_title(conversation, Some("Independent pending title"))
            .unwrap();
    }
    let now = crate::runtime::current_unix_millis();
    assert!(
        !scene
            .service
            .resume_budget_title_claim(&first.2, now)
            .unwrap()
    );
    assert!(
        !scene
            .service
            .resume_budget_title_claim(&second.2, now)
            .unwrap()
    );
    scene
        .service
        .cancel_agent_session_title_task(&conversations[2]);
    let overtaking = scene
        .service
        .resume_budget_title_claim(&second.2, now + 1)
        .unwrap();
    let newcomer = scene.service.schedule_runtime_agent_session_title(
        &conversations[2],
        Some("Cannot overtake original wait"),
    );
    let oldest = scene
        .service
        .resume_budget_title_claim(&first.2, now + 2)
        .unwrap();
    let duplicate = scene
        .service
        .resume_budget_title_claim(&first.2, now + 3)
        .unwrap();
    scene
        .service
        .cancel_agent_session_title_task(&conversations[0]);
    let successor = scene
        .service
        .resume_budget_title_claim(&second.2, now + 4)
        .unwrap();
    let stopped = scene
        .service
        .resume_budget_title_claim(&first.2, now + 5)
        .is_err();
    drop((first, second, waiter));
    drop(scene);
    assert!(!overtaking && newcomer.is_err());
    assert!(oldest && duplicate && successor && stopped);
}

#[tokio::test]
async fn runtime_budget_title_dead_ready_waiter_does_not_block_new_admission() {
    let (mut scene, primary, first_conversation) = super::auxiliary::setup();
    let pane = scene
        .service
        .session_mut_for_tests()
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Horizontal)
        .unwrap();
    let second_conversation = scene
        .service
        .agent_shell_store_mut()
        .enter_or_resume(pane.as_str())
        .unwrap()
        .session_id
        .clone();
    let waiter = Arc::new(());
    let first = parked_title(&mut scene, &first_conversation, &waiter).await;
    scene
        .service
        .schedule_runtime_agent_session_title(
            &second_conversation,
            Some("Independent active title"),
        )
        .unwrap();
    // One free slot remains, so fill it with the original before parking a
    // genuinely new hold. A dead positive hold is not quiescence proof.
    let now = crate::runtime::current_unix_millis();
    assert!(
        scene
            .service
            .resume_budget_title_claim(&first.2, now)
            .unwrap()
    );
    let _ = first.1.retry_held().await.unwrap();
    let capture = first.1.capture_held().await.unwrap();
    scene
        .service
        .park_budget_title_claim(capture.clone(), Arc::downgrade(&waiter), now + 1)
        .unwrap();
    drop(waiter);
    let quiescent = scene
        .service
        .budget_title_claim_is_quiescent(&first_conversation);
    let refused = scene
        .service
        .resume_budget_title_claim(&capture, now + 2)
        .is_err();
    scene
        .service
        .cancel_agent_session_title_task(&first_conversation);
    let recovered = scene
        .service
        .schedule_runtime_agent_session_title(&first_conversation, Some("New current title"));
    drop((capture, first));
    drop(scene);
    assert!(!quiescent && refused && recovered.is_ok());
}
