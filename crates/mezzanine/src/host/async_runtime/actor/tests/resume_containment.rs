//! Stale resume refusal crosses the real command service and actor without
//! replaying effects or poisoning subsequent commands. Fixtures are generated
//! offline; explicit post-read gates make geometry invalidation deterministic.

use super::super::*;
use super::replay_epoch_publication::setup;
use crate::host::async_runtime::{
    AsyncAgentProviderServiceConfig, run_async_agent_command_service,
};
use crate::runtime::{RuntimeAgentCommandLifecyclePhase, RuntimeSessionService};
use crate::test_support::replay_epoch::CONVERSATION;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Unchanged,
    Resize,
    Stop,
    IngressLost,
}

async fn notified(notify: &tokio::sync::Notify) {
    tokio::time::timeout(Duration::from_secs(10), notify.notified())
        .await
        .expect("command phase must finish");
}

async fn worker_case(operation: Operation) {
    let (_fixture, mut service, primary) = setup();
    // Resume a different saved source so refusal must retain the old binding.
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "original-live", 0)
        .unwrap();
    let other = service
        .session_mut_for_tests()
        .split_active_pane(&primary, mez_mux::layout::SplitDirection::Vertical)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume(other.as_str())
        .unwrap();
    let other_conversation = service
        .agent_shell_store()
        .get(other.as_str())
        .unwrap()
        .session_id
        .clone();
    service
        .session_mut_for_tests()
        .select_pane(&primary, "%1")
        .unwrap();
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let done = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(tokio::sync::Notify::new());
    service.set_deferred_agent_command_probe_for_tests(started.clone(), release.clone());
    service.set_deferred_agent_command_completion_probe_for_tests(done.clone(), settled.clone());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let client = async {
        let worker_handle = handle.clone();
        let stopping = stop.clone();
        let worker = tokio::spawn(async move {
            run_async_agent_command_service(
                &worker_handle,
                AsyncAgentProviderServiceConfig::new(1)
                    .unwrap()
                    .with_idle_interval(Duration::from_millis(2))
                    .unwrap(),
                move |_, state| {
                    stopping.load(Ordering::Acquire)
                        || matches!(state, crate::runtime::RuntimeLifecycleState::Stopping)
                },
            )
            .await
        });
        handle
            .execute_agent_shell_command(primary.clone(), format!("/resume {CONVERSATION}"))
            .await
            .unwrap();
        notified(&started).await;
        release.notify_one();
        notified(&done).await;
        notified(&started).await; // Actual projection is now held before adoption.
        match operation {
            Operation::Resize => {
                handle
                    .resize_attached_primary_terminal(primary.clone(), Size::new(120, 36).unwrap())
                    .await
                    .unwrap();
            }
            Operation::Stop => {
                handle
                    .execute_agent_shell_command(primary.clone(), "/stop".into())
                    .await
                    .unwrap();
            }
            Operation::IngressLost => {
                handle.shutdown().await.unwrap();
            }
            Operation::Unchanged => {}
        }
        release.notify_one();
        if operation == Operation::IngressLost {
            let result = tokio::time::timeout(Duration::from_secs(10), worker)
                .await
                .unwrap()
                .unwrap();
            assert!(
                result.is_err(),
                "actor loss must not become a local success"
            );
            return;
        }
        notified(&settled).await;
        let view = handle
            .render_client_view(
                ClientViewRole::Primary,
                Size::new(120, 36).unwrap(),
                crate::host::terminal::TerminalClientLoopConfig::default(),
            )
            .await
            .unwrap()
            .unwrap();
        let text = view.lines.join("\n");
        if operation == Operation::Resize {
            assert!(text.contains("direct resume projection is stale"), "{text}");
        }
        assert!(!text.contains("command running"), "{text}");
        // The same attachment and command service must process another command
        // in an unrelated pane after the first worker has returned.
        handle
            .execute_terminal_command(primary.clone(), format!("select-pane -t {other}"))
            .await
            .unwrap();
        handle
            .execute_agent_shell_command(primary.clone(), "/list-personalities".into())
            .await
            .unwrap();
        notified(&started).await;
        release.notify_one();
        notified(&done).await;
        notified(&settled).await;
        stop.store(true, Ordering::Release);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(10), worker)
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            2
        );
        handle.shutdown().await.unwrap();
    };
    let ((), exit) = tokio::join!(client, actor.run());
    let service = exit.service;
    let current = service.agent_shell_store().get("%1").unwrap();
    assert_eq!(
        current.session_id,
        if operation == Operation::Unchanged {
            CONVERSATION
        } else {
            "original-live"
        }
    );
    if operation != Operation::IngressLost {
        assert_eq!(
            service
                .agent_shell_store()
                .get(other.as_str())
                .unwrap()
                .session_id,
            other_conversation
        );
        assert_eq!(
            service.agent_command_lifecycle_phase_for_tests(other.as_str()),
            Some(RuntimeAgentCommandLifecyclePhase::Completed)
        );
        assert_eq!(
            service.agent_command_lifecycle_phase_for_tests("%1"),
            Some(match operation {
                Operation::Unchanged => RuntimeAgentCommandLifecyclePhase::Completed,
                Operation::Resize => RuntimeAgentCommandLifecyclePhase::Failed,
                Operation::Stop => RuntimeAgentCommandLifecyclePhase::Cancelled,
                Operation::IngressLost => unreachable!(),
            })
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn resume_containment_real_worker_resize_is_command_local() {
    worker_case(Operation::Resize).await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_containment_real_worker_unchanged_adopts() {
    worker_case(Operation::Unchanged).await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_containment_real_worker_stop_discards() {
    worker_case(Operation::Stop).await;
}

#[tokio::test(flavor = "current_thread")]
async fn resume_containment_real_worker_ingress_loss_is_not_swallowed() {
    worker_case(Operation::IngressLost).await;
}

/// Each captured renderer input still rejects independently, and duplicate old
/// completion cannot settle the newer command's generation.
#[test]
fn resume_containment_freshness_and_duplicate_generation() {
    for field in 0..5 {
        let (_fixture, mut service, primary) = setup();
        service
            .agent_shell_store_mut()
            .bind_conversation("%1", "original-live", 0)
            .unwrap();
        service
            .execute_agent_shell_command(&primary, &format!("/resume {CONVERSATION}"))
            .unwrap();
        let dispatch = service.take_pending_deferred_agent_commands().remove(0);
        let work = service
            .claim_agent_command_work(
                &primary,
                "%1",
                &dispatch.command,
                &dispatch.input,
                dispatch.claim_generation,
                &dispatch.conversation_id,
            )
            .unwrap()
            .unwrap();
        let mut outcome = RuntimeSessionService::execute_deferred_agent_command(&work);
        let crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { read, .. } =
            &mut outcome
        else {
            panic!("fixture must produce a valid resume projection");
        };
        let projection = read.projection.as_mut().unwrap();
        match field {
            0 => projection.history_limit += 1,
            1 => projection.history_rotate_lines += 1,
            2 => projection.presentation_source_revision = Some(u64::MAX),
            3 => projection.size = Size::new(121, 37).unwrap(),
            4 => {
                let mut layers = service.config_layers().to_vec();
                layers[0]
                    .text
                    .push_str("\n[terminal]\nagent_log_timestamps=true\n");
                service.replace_config_layers(layers).unwrap();
            }
            _ => unreachable!(),
        }
        let crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume { store, read } = outcome
        else {
            unreachable!()
        };
        let duplicate = || crate::runtime::RuntimeAgentCommandAsyncOutcome::DirectResume {
            store: store.clone(),
            read: read.clone(),
        };
        assert!(
            service
                .complete_agent_command_work(&work, duplicate())
                .unwrap()
        );
        assert_eq!(
            service.agent_command_lifecycle_phase_for_tests("%1"),
            Some(RuntimeAgentCommandLifecyclePhase::Failed)
        );
        assert_eq!(
            service.agent_shell_store().get("%1").unwrap().session_id,
            "original-live"
        );
        assert!(
            !service
                .complete_agent_command_work(&work, duplicate())
                .unwrap()
        );
        service
            .execute_agent_shell_command(&primary, "/list-personalities")
            .unwrap();
        assert!(
            !service
                .complete_agent_command_work(&work, duplicate())
                .unwrap()
        );
        assert_eq!(
            service.agent_command_lifecycle_phase_for_tests("%1"),
            Some(RuntimeAgentCommandLifecyclePhase::Queued)
        );
    }
}
