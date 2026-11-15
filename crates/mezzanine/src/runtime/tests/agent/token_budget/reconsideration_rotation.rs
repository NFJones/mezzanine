//! Real ordinary/title/memory claims compete on one native account. Selection
//! is not family resume or send authority; these actor tests qualify ordering
//! and stale readiness, not physical three-family transport or initial fairness.

use crate::runtime::{
    RuntimeBudgetAdmissionWork, RuntimeBudgetAttemptAdmission, RuntimeBudgetAuxiliarySource,
    RuntimeBudgetHeldAttempt,
};
use crate::storage::token_usage::budget::receipts::BudgetAttemptPurpose;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeadState {
    Ready,
    Dropped,
    Revoked,
    Paused,
}

#[tokio::test]
async fn runtime_budget_reconsideration_three_families_rotate_without_poll_overtaking() {
    Box::pin(qualify(HeadState::Ready)).await;
}

#[tokio::test]
async fn runtime_budget_reconsideration_dead_ready_head_does_not_strand_another_family() {
    Box::pin(qualify(HeadState::Dropped)).await;
}

#[tokio::test]
async fn runtime_budget_reconsideration_revoked_ready_head_does_not_strand_another_family() {
    Box::pin(qualify(HeadState::Revoked)).await;
}

#[tokio::test]
async fn runtime_budget_reconsideration_paused_heads_allow_child_progress_and_ordered_return() {
    Box::pin(qualify(HeadState::Paused)).await;
}

async fn browser_input(
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
    primary: &mez_core::ids::ClientId,
    bytes: &[u8],
) {
    use crate::host::terminal::{AttachedTerminalClientStepPlan, TerminalClientLoopAction};
    handle
        .apply_attached_terminal_step_plan(
            primary.clone(),
            AttachedTerminalClientStepPlan {
                actions: vec![TerminalClientLoopAction::ForwardToPane(bytes.to_vec())],
                output_lines: Vec::new(),
                output_line_style_spans: Vec::new(),
                input_hangup: false,
                output_hangup: false,
                error_roles: Vec::new(),
            },
        )
        .await
        .unwrap();
}

async fn browser_view(handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle) -> String {
    handle
        .render_client_view(
            mez_mux::presentation::ClientViewRole::Primary,
            mez_mux::layout::Size::new(120, 40).unwrap(),
            crate::host::terminal::TerminalClientLoopConfig::default(),
        )
        .await
        .unwrap()
        .unwrap()
        .lines
        .join("\n")
}

async fn qualify(stale: HeadState) {
    let (mut scene, ordinary, ordinary_work, ordinary_hold) =
        super::request::provider_wait::held().await;
    // Auxiliary config introduces real pane path-resolution requirements.
    // Establish the ordinary fixture's original claim before adding it.
    super::auxiliary::configure(&mut scene);
    let primary = scene
        .service
        .attach_primary("primary", true, super::Size::new(120, 30).unwrap(), 120)
        .unwrap();
    let conversation = ordinary.turn.conversation_id.clone();
    // Enabling generated titles may queue the current conversation already.
    // Retire that unclaimed fixture task before making the explicit source.
    scene.service.cancel_agent_session_title_task(&conversation);
    let mut futures: Vec<Option<Arc<()>>> = (0..3).map(|_| Some(Arc::new(()))).collect();
    let title = super::title_capacity::parked_title(
        &mut scene,
        &conversation,
        futures[1].as_ref().unwrap(),
    )
    .await;
    scene
        .service
        .configure_subagent_policy(8, 8, 8, 8, super::SubagentWaitPolicy::Detach);
    let spawned = scene
        .service
        .spawn_runtime_persistent_subagent_session_owned(
            super::SubagentSpawnRequest {
                parent_agent_id: ordinary.turn.agent_id.clone(),
                requested_role: "worker".into(),
                placement: "new-pane".into(),
                cooperation_mode: super::CooperationMode::ExploreOnly,
                cooperation_mode_defaulted: false,
                read_scopes: Vec::new(),
                read_scopes_defaulted: false,
                write_scopes: Vec::new(),
                write_scopes_defaulted: false,
                session_mode: mez_agent::SubagentSessionMode::New,
                initial_model_size: None,
                initial_reasoning_effort: None,
                task_prompt: String::new(),
                explicit_user_approval: false,
                skip_initial_turn: true,
            },
            super::RuntimeSubagentPlacement::NewPane {
                direction: super::SplitDirection::Vertical,
                select: false,
            },
            &conversation,
            "independent same-root memory contender",
        )
        .unwrap();
    let spawned: serde_json::Value = serde_json::from_str(&spawned).unwrap();
    let child_agent = spawned["agent"]["id"].as_str().unwrap();
    let memory_pane = child_agent.strip_prefix("agent-").unwrap().to_owned();
    super::descendants::ready_child(&mut scene, &memory_pane);
    let parent_window = scene
        .service
        .session
        .active_window_for(&primary)
        .unwrap()
        .id
        .clone();
    let memory_window = spawned["pane"]["window_id"].as_str().unwrap();
    scene
        .service
        .execute_terminal_command(&primary, &format!("select-window -t {memory_window}"))
        .unwrap();
    scene
        .service
        .execute_agent_shell_command(
            &primary,
            "/remember Qualify fair original memory readiness.",
        )
        .unwrap();
    let observation = scene
        .service
        .pending_agent_remember_observation_id(&memory_pane)
        .unwrap()
        .to_owned();
    super::auxiliary::prepare(
        &mut scene,
        &RuntimeBudgetAuxiliarySource::Remember {
            pane: memory_pane.clone(),
            observation: observation.clone(),
        },
    )
    .await
    .unwrap();
    let memory = scene
        .service
        .claim_agent_remember_task(&memory_pane, &observation)
        .unwrap()
        .unwrap();
    scene
        .service
        .execute_terminal_command(&primary, &format!("select-window -t {parent_window}"))
        .unwrap();
    let memory_admission = memory
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
        memory_admission.claim().await.unwrap(),
        RuntimeBudgetAttemptAdmission::Held(_)
    ));
    let memory_hold = memory_admission.capture_held().await.unwrap();
    assert_eq!(
        ordinary.budget_owner.as_ref().unwrap().member.root,
        title.0.budget_owner.as_ref().unwrap().member.root
    );
    assert_eq!(
        ordinary.budget_owner.as_ref().unwrap().member.root,
        memory.budget_owner.as_ref().unwrap().member.root
    );
    let captures: Vec<RuntimeBudgetHeldAttempt> = vec![ordinary_hold, title.2.clone(), memory_hold];
    let admissions: Vec<RuntimeBudgetAdmissionWork> = vec![
        ordinary_work.admission_owner(),
        title.1.clone(),
        memory_admission,
    ];
    if stale == HeadState::Paused {
        // Browser controls require a genuine native pane incarnation. This
        // disposable shell belongs to Scene and is terminated before oracles.
        scene
            .service
            .start_initial_pane_process_with_start_directory(Some("/bin/sh"), &scene._root)
            .unwrap();
    }
    let service = std::mem::replace(&mut scene.service, super::test_runtime_service());
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        handle
            .park_budget_provider_claim(
                mez_core::ids::AgentId::opaque(ordinary.turn.agent_id.clone()).unwrap(),
                ordinary.turn.turn_id.clone(),
                7,
                captures[0].clone(),
            )
            .await
            .unwrap();
        handle
            .park_budget_auxiliary_claim(
                captures[2].clone(),
                Arc::downgrade(futures[2].as_ref().unwrap()),
            )
            .await
            .unwrap();
        let first_window = Arc::new(());
        let mut checks = vec![
            handle
                .select_budget_reconsideration(
                    captures[0].clone(),
                    Arc::downgrade(futures[0].as_ref().unwrap()),
                    Arc::downgrade(&first_window),
                )
                .await
                .unwrap(),
        ];
        for index in [1, 2] {
            let window = Arc::new(());
            checks.push(
                !handle
                    .select_budget_reconsideration(
                        captures[index].clone(),
                        Arc::downgrade(futures[index].as_ref().unwrap()),
                        Arc::downgrade(&window),
                    )
                    .await
                    .unwrap(),
            );
        }
        drop(first_window);
        // The first selection rotated ordinary before the other families
        // registered, so its next position still precedes theirs. Consume
        // that position now: title is then the actual oldest ready head.
        let enrolled_window = Arc::new(());
        checks.push(
            handle
                .select_budget_reconsideration(
                    captures[0].clone(),
                    Arc::downgrade(futures[0].as_ref().unwrap()),
                    Arc::downgrade(&enrolled_window),
                )
                .await
                .unwrap(),
        );
        drop(enrolled_window);
        if stale == HeadState::Dropped {
            futures[1].take();
        }
        if stale == HeadState::Revoked {
            let _ = admissions[1].retry_held().await.unwrap();
        }
        if stale == HeadState::Paused {
            browser_input(&handle, &primary, b"/list-agents --all\r").await;
            let _ = browser_view(&handle).await;
            browser_input(&handle, &primary, b"/agent-%1\r").await;
            let _ = browser_view(&handle).await;
            browser_input(&handle, &primary, b"p").await;
            let paused = browser_view(&handle).await;
            let cell = |column| -> String {
                paused
                    .lines()
                    .skip_while(|line| !line.contains('├'))
                    .skip(1)
                    .take_while(|line| line.contains('│'))
                    .filter_map(|line| line.split('│').nth(column))
                    .map(str::trim)
                    .collect()
            };
            checks.push(cell(1) == "agent-%1" && cell(7) == "paused");
        }
        let mut observed = Vec::new();
        let mut expected: Vec<usize> = if stale == HeadState::Ready {
            (0..12).map(|i| (i + 1) % 3).collect()
        } else if stale == HeadState::Paused {
            vec![2; 12]
        } else {
            (0..12).map(|i| if i % 2 == 0 { 2 } else { 0 }).collect()
        };
        for index in &expected {
            // Ask the most recently selected contender first. It must not
            // overtake an older live original merely by polling more often.
            let wrong = if stale == HeadState::Ready {
                (index + 2) % 3
            } else {
                2 - index
            };
            let window = Arc::new(());
            checks.push(
                !handle
                    .select_budget_reconsideration(
                        captures[wrong].clone(),
                        Arc::downgrade(futures[wrong].as_ref().unwrap()),
                        Arc::downgrade(&window),
                    )
                    .await
                    .unwrap(),
            );
            let selected = handle
                .select_budget_reconsideration(
                    captures[*index].clone(),
                    Arc::downgrade(futures[*index].as_ref().unwrap()),
                    Arc::downgrade(&window),
                )
                .await
                .unwrap();
            if selected {
                observed.push(*index);
            }
        }
        if stale == HeadState::Paused {
            // Both parent claims were filtered while paused. Resume through
            // the same captured browser target, then enroll them behind the
            // child's existing position, never by creating another request.
            browser_input(&handle, &primary, b"p").await;
            browser_input(&handle, &primary, b"\x1b").await;
            for index in [1, 0] {
                let window = Arc::new(());
                checks.push(
                    !handle
                        .select_budget_reconsideration(
                            captures[index].clone(),
                            Arc::downgrade(futures[index].as_ref().unwrap()),
                            Arc::downgrade(&window),
                        )
                        .await
                        .unwrap(),
                );
            }
            let window = Arc::new(());
            checks.push(
                handle
                    .select_budget_reconsideration(
                        captures[2].clone(),
                        Arc::downgrade(futures[2].as_ref().unwrap()),
                        Arc::downgrade(&window),
                    )
                    .await
                    .unwrap(),
            );
            drop(window);
            for index in [1, 0, 2, 1, 0, 2] {
                let window = Arc::new(());
                if handle
                    .select_budget_reconsideration(
                        captures[index].clone(),
                        Arc::downgrade(futures[index].as_ref().unwrap()),
                        Arc::downgrade(&window),
                    )
                    .await
                    .unwrap()
                {
                    observed.push(index);
                }
                expected.push(index);
            }
        }
        handle
            .execute_agent_shell_command(primary, "/stop".into())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();
        (checks, observed, expected)
    };
    let ((checks, observed, expected), exit) = tokio::join!(client, actor.run());
    scene.service = exit.service;
    drop((
        handle,
        captures,
        admissions,
        futures,
        ordinary_work,
        ordinary,
        title,
        memory,
    ));
    drop(scene); // Retire exact owned native scene/root before final oracles.
    assert!(
        checks.iter().all(|check| *check),
        "a newer poll overtook readiness: {checks:?}; selected={observed:?}; expected={expected:?}"
    );
    assert_eq!(observed, expected);
}
