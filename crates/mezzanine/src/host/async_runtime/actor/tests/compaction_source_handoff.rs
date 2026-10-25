//! Exact-generation auxiliary source recovery under finite committed pressure.
//!
//! Real synchronous `/compact` admission builds the pending task from generated
//! durable history. Only the human-pause fixture owns a live pane root for
//! lifecycle authority; no inference, native action or persistence worker runs.
//! A recovered descriptor transfers neither a claim nor timer authority.

use super::super::*;

/// Sends the real actor claim request, including mandatory lease admission.
/// No returned provider dispatch is executed by these custody-only fixtures.
async fn claim(
    actor: &mut AsyncRuntimeSessionActor,
    generation: u64,
) -> Result<Option<crate::runtime::RuntimeAgentCompactionDispatch>> {
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimAgentCompactionTask {
            pane_id: "%1".into(),
            task_generation: generation,
            reply,
        })
        .await;
    response.await.unwrap()
}

/// Offers the exact refused generation after one actual timer-lane drain.
fn refuse_and_offer(actor: &mut AsyncRuntimeSessionActor, generation: u64) {
    let effects = actor
        .pending_provider_dispatch_side_effects()
        .unwrap()
        .into_iter()
        .filter(|effect| matches!(effect, RuntimeSideEffect::DispatchAgentCompaction { .. }))
        .collect();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert!(
        matches!(actor.drain_agent_provider_dispatch_side_effects(1).unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation)
    );
}

/// Owns only generated transcript/auth storage; no worker accesses it after the
/// directly driven actor is dropped. Cleanup also runs on an assertion unwind.
struct FixtureRoot(std::path::PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("compaction handoff fixture storage cleanup failed: {error}");
        }
    }
}

/// Admits actual manual compaction before the actor enables asynchronous source
/// preparation. No synthetic task/claim is inserted. Saturation occurs only
/// after startup descriptors have drained, leaving exactly one compaction source.
fn pending_compaction_at_capacity() -> (FixtureRoot, AsyncRuntimeSessionActor, ClientId, u64) {
    pending_compaction_with_lifecycle_owner(false)
}

/// Human pause additionally requires actual registration and a kernel-qualified
/// live pane root. Only that control opts in; ordinary custody tests spawn none.
fn pending_compaction_with_lifecycle_owner(
    lifecycle_owner: bool,
) -> (FixtureRoot, AsyncRuntimeSessionActor, ClientId, u64) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let root = FixtureRoot(std::env::temp_dir().join(format!(
        "mez-compaction-handoff-{:032x}",
        rand::random::<u128>()
    )));
    let store = crate::storage::transcript::AgentTranscriptStore::new(root.0.clone());
    for sequence in 1..=3 {
        store
            .append(&mez_agent::transcript::TranscriptEntry {
                conversation_id: "handoff-compaction".into(),
                sequence,
                created_at_unix_seconds: sequence,
                role: mez_agent::transcript::TranscriptRole::Assistant,
                turn_id: format!("completed-{sequence}"),
                agent_id: "agent-%1".into(),
                pane_id: "%1".into(),
                content: format!("completed source {sequence}"),
            })
            .unwrap();
    }
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_agent_transcript_store(store);
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.0),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "compaction-handoff".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"pane\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("compaction-primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .agent_shell_store_mut()
        .bind_conversation("%1", "handoff-compaction", 3)
        .unwrap();
    if lifecycle_owner {
        service.start_initial_pane_process(None).unwrap();
        service
            .execute_agent_shell_command(&primary, "register compaction fixture")
            .unwrap();
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
    let admitted = service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    let generation = service
        .pending_agent_compaction_task_generation("%1")
        .unwrap_or_else(|| panic!("actual compaction task absent: {admitted}"));
    let (_, mut actor) = AsyncRuntimeSessionActor::new(
        service,
        AsyncRuntimeActorConfig {
            side_effect_buffer: 1,
            ..Default::default()
        },
    )
    .unwrap();
    actor.drain_runtime_side_effects(64).unwrap();
    actor.drain_timer_side_effects(64).unwrap();
    actor.drain_persistence_side_effects(64).unwrap();
    let capacity = 1 + actor.committed_side_effect_overshoot();
    actor
        .admit_committed_side_effects(
            (0..capacity)
                .map(|index| RuntimeSideEffect::ScheduleTimer {
                    key: RuntimeTimerKey::new(
                        RuntimeTimerKind::PanePipeHealth,
                        format!("occupied-{index}"),
                        1,
                    ),
                    delay_ms: 25,
                })
                .collect(),
        )
        .unwrap();
    (root, actor, primary, generation)
}

/// Admits a fresh asynchronous operation and performs only its immutable source
/// phase. Returns real request work without executing inference or inserting a
/// fabricated task; the caller drives the actual request-completion boundary.
fn prepare_manual_request(
    actor: &mut AsyncRuntimeSessionActor,
    primary: &ClientId,
) -> crate::runtime::RuntimeManualCompactionRequestWork {
    actor
        .service
        .execute_agent_shell_command(primary, "/stop")
        .unwrap();
    let admitted = actor
        .service
        .execute_agent_shell_command(primary, "/compact")
        .unwrap();
    assert!(admitted.contains("state=preparing"));
    let source = actor
        .service
        .take_manual_compaction_preparations()
        .remove(0);
    let result = source.execute_source();
    assert!(
        actor
            .service
            .complete_manual_compaction_preparation(&source, result)
            .unwrap()
    );
    actor.service.take_manual_compaction_requests().remove(0)
}

/// Source adoption must start the real off-actor request renderer even with a
/// full transient lane. Collect its actual bounded-ingress completion and adopt
/// it through the ordinary handler; draining capacity then recovers one exact
/// provider descriptor, not a second source read or a replayed slash command.
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_completion_drives_two_phase_worker_under_pressure() {
    let (_root, mut actor, primary, _) = pending_compaction_at_capacity();
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    let admitted = actor
        .service
        .execute_agent_shell_command(&primary, "/compact")
        .unwrap();
    assert!(admitted.contains("state=preparing"));
    let work = actor
        .service
        .take_manual_compaction_preparations()
        .remove(0);
    let result = work.execute_source();
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteManualCompactionPreparation {
            work,
            result,
        }),
    )
    .await;
    assert!(actor.service.agent_is_compacting("%1"));
    assert!(
        actor
            .service
            .pending_agent_compaction_task_generation("%1")
            .is_none()
    );
    let completion = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let notify = actor.request_ingress_notify.clone();
            let notified = notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let request = actor
                .request_ingress
                .lock()
                .unwrap()
                .normal_requests
                .pop_front();
            if let Some(request) = request {
                break request.envelope.request;
            }
            notified.await;
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(&completion, AsyncRuntimeRequest::CompleteManualCompactionRequest { result, .. } if result.is_ok())
    );
    Box::pin(actor.handle_request(completion)).await;
    let generation = actor
        .service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(
        actor
            .refused_dispatch_sources
            .contains(&SourceDispatch::Compaction("%1".into(), generation))
    );
    actor.drain_timer_side_effects(64).unwrap();
    let effects = actor
        .drain_agent_provider_dispatch_side_effects(64)
        .unwrap();
    assert!(
        matches!(effects.as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation)
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// When the adopted compactor fits the existing reserve, completion queues it
/// immediately rather than waiting for a future producer poll. This allowance
/// does not claim a worker or bypass the separate mandatory completion lease.
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_completion_uses_available_reserve() {
    let (_root, mut actor, primary, _) = pending_compaction_at_capacity();
    let work = prepare_manual_request(&mut actor, &primary);
    let result = work.execute_request();
    actor.drain_timer_side_effects(2).unwrap();
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteManualCompactionRequest {
            work: Box::new(work),
            result: Box::new(result),
        }),
    )
    .await;
    let generation = actor
        .service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(actor.compaction_dispatch_is_already_queued("%1", generation));
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(
        !actor
            .service
            .agent_compaction_task_is_claimed("%1", generation)
    );
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .len(),
        1
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Worker errors retire the exact logical owner without enrolling nonexistent
/// provider work. Exercise both typed completion messages under full pressure;
/// storage/construction failure must not become a fabricated successful request.
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_completion_worker_errors_do_not_enrol_dispatch() {
    for source_phase in [false, true] {
        let (_root, mut actor, primary, _) = pending_compaction_at_capacity();
        let failure = crate::error::MezError::invalid_state("generated preparation failure");
        let request = if source_phase {
            actor
                .service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            let admitted = actor
                .service
                .execute_agent_shell_command(&primary, "/compact")
                .unwrap();
            assert!(admitted.contains("state=preparing"));
            let work = actor
                .service
                .take_manual_compaction_preparations()
                .remove(0);
            AsyncRuntimeRequest::CompleteManualCompactionPreparation {
                work,
                result: Err(failure),
            }
        } else {
            let work = prepare_manual_request(&mut actor, &primary);
            AsyncRuntimeRequest::CompleteManualCompactionRequest {
                work: Box::new(work),
                result: Box::new(Err(failure)),
            }
        };
        Box::pin(actor.handle_request(request)).await;
        assert!(!actor.service.agent_is_compacting("%1"));
        assert!(
            actor
                .service
                .pending_agent_compaction_task_generation("%1")
                .is_none()
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        actor.drain_timer_side_effects(64).unwrap();
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
    }
}

/// Cancelled source/request callbacks cannot enrol a separately admitted newer
/// compactor even in the same pane. Keep the new request genuinely pending, then
/// deliver old work through each real completion handler before freeing capacity.
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_completion_stale_callbacks_do_not_enrol_replacement() {
    for source_phase in [false, true] {
        let (_root, mut actor, primary, _) = pending_compaction_at_capacity();
        let work = prepare_manual_request(&mut actor, &primary);
        let old_source = work.owner.clone();
        let old_source_result = old_source.execute_source();
        let old_request_result = work.execute_request();
        let replacement = prepare_manual_request(&mut actor, &primary);
        assert!(
            actor
                .service
                .complete_manual_compaction_request(&replacement, replacement.execute_request())
                .unwrap()
        );
        let generation = actor
            .service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        let request = if source_phase {
            AsyncRuntimeRequest::CompleteManualCompactionPreparation {
                work: old_source,
                result: old_source_result,
            }
        } else {
            AsyncRuntimeRequest::CompleteManualCompactionRequest {
                work: Box::new(work),
                result: Box::new(old_request_result),
            }
        };
        Box::pin(actor.handle_request(request)).await;
        assert_eq!(
            actor.service.pending_agent_compaction_task_generation("%1"),
            Some(generation)
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        actor.drain_timer_side_effects(64).unwrap();
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        assert!(
            matches!(actor.pending_provider_dispatch_side_effects().unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation)
        );
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
}

/// An ordinary pending title is a different producer, even in the same pane.
/// Its strict refusal must not acquire committed recovery merely because manual
/// request completion adopts a compactor. Both sources are genuinely admitted;
/// only the compactor may be offered after real capacity progress.
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_completion_excludes_unrelated_pending_title() {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let (_root, mut actor, primary, _) = pending_compaction_at_capacity();
    actor.service.replace_config_layers(vec![ConfigLayer {
        name: "compaction-title-isolation".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"pane\"\nsession_title_policy=\"generated\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    assert_eq!(
        actor.service.schedule_runtime_agent_session_title(
            "handoff-compaction",
            Some("Maintain isolated completion source ownership.")
        ),
        Ok(())
    );
    let title = actor
        .service
        .pending_agent_session_title_dispatch_id("handoff-compaction")
        .unwrap()
        .to_string();
    let work = prepare_manual_request(&mut actor, &primary);
    let title_effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(
        matches!(title_effects.as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { dispatch_id, .. }] if dispatch_id == &title)
    );
    assert!(actor.queue_runtime_side_effects(title_effects).is_err());
    let result = work.execute_request();
    Box::pin(
        actor.handle_request(AsyncRuntimeRequest::CompleteManualCompactionRequest {
            work: Box::new(work),
            result: Box::new(result),
        }),
    )
    .await;
    let generation = actor
        .service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert_eq!(
        actor
            .refused_dispatch_sources
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
        vec![SourceDispatch::Compaction("%1".into(), generation)]
    );
    actor.drain_timer_side_effects(64).unwrap();
    let offered = actor
        .drain_agent_provider_dispatch_side_effects(64)
        .unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation)
    );
    assert!(
        matches!(actor.pending_provider_dispatch_side_effects().unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { dispatch_id, .. }] if dispatch_id == &title)
    );
    assert_eq!(
        actor
            .service
            .pending_agent_session_title_dispatch_id("handoff-compaction"),
        Some(title.as_str())
    );
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    actor
        .service
        .cancel_agent_session_title_task("handoff-compaction");
}

/// The asynchronous request-completion message has already adopted real source
/// and a rendered request. Its accepted provider task must survive finite queue
/// pressure even though no caller is awaiting another admission reply. Exercise
/// the actual completion handler rather than manually enrolling its descriptor;
/// real drain progress alone must recover it through either worker adapter.
#[tokio::test(flavor = "current_thread")]
async fn manual_compaction_completion_recovers_accepted_dispatch() {
    for compatibility in [false, true] {
        let (_root, mut actor, primary, old) = pending_compaction_at_capacity();
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        let admitted = actor
            .service
            .execute_agent_shell_command(&primary, "/compact")
            .unwrap();
        assert!(admitted.contains("state=preparing"));
        let source = actor
            .service
            .take_manual_compaction_preparations()
            .remove(0);
        let result = source.execute_source();
        assert!(
            actor
                .service
                .complete_manual_compaction_preparation(&source, result)
                .unwrap()
        );
        let work = actor.service.take_manual_compaction_requests().remove(0);
        let result = work.execute_request();
        assert!(result.is_ok());
        actor
            .handle_request(AsyncRuntimeRequest::CompleteManualCompactionRequest {
                work: Box::new(work),
                result: Box::new(result),
            })
            .await;
        let generation = actor
            .service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        assert!(generation > old);
        assert!(
            actor
                .refused_dispatch_sources
                .contains(&SourceDispatch::Compaction("%1".into(), generation))
        );
        // Completion also produced status obligations. Preserve the shared
        // transfer fairness instead of requiring the compactor to take slot one.
        actor.drain_timer_side_effects(64).unwrap();
        let offered = if compatibility {
            actor.drain_runtime_side_effects(1).unwrap()
        } else {
            actor.drain_agent_provider_dispatch_side_effects(1).unwrap()
        };
        assert!(
            matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation),
            "{offered:?}"
        );
        assert!(
            actor
                .pending_provider_dispatch_side_effects()
                .unwrap()
                .is_empty()
        );
        actor.drain_timer_side_effects(64).unwrap();
        let dispatch = claim(&mut actor, generation).await.unwrap().unwrap();
        assert_eq!(dispatch.task.task_generation, generation);
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
}

/// Committed refusal must recover on real lane progress with no new event. Both
/// drain adapters fence the offer until actual claim; a successful claim admits
/// exactly one existing completion lease and cannot recover or claim twice.
#[tokio::test(flavor = "current_thread")]
async fn refused_compaction_recovers_and_claims_once() {
    for compatibility in [false, true] {
        let (_root, mut actor, primary, generation) = pending_compaction_at_capacity();
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(
            matches!(effects.as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation)
        );
        assert!(actor.admit_committed_side_effects(effects).is_err());
        actor.drain_timer_side_effects(1).unwrap();
        let offered = if compatibility {
            actor.drain_runtime_side_effects(1).unwrap()
        } else {
            actor.drain_agent_provider_dispatch_side_effects(1).unwrap()
        };
        assert!(
            matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { pane_id, task_generation }] if pane_id == "%1" && *task_generation == generation),
            "{offered:?}"
        );
        assert!(
            actor
                .pending_provider_dispatch_side_effects()
                .unwrap()
                .is_empty()
        );
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(1)
                .unwrap()
                .is_empty()
        );
        actor.drain_timer_side_effects(64).unwrap();
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimAgentCompactionTask {
                pane_id: "%1".into(),
                task_generation: generation,
                reply,
            })
            .await;
        let dispatch = response
            .await
            .unwrap()
            .unwrap()
            .expect("actual recovered compaction claim");
        assert_eq!(dispatch.task.task_generation, generation);
        assert!(
            actor
                .service
                .agent_compaction_task_is_claimed("%1", generation)
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        let timers = actor.drain_timer_side_effects(64).unwrap();
        assert_eq!(timers.iter().filter(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::CompactionClaim && key.owner_id == "%1" && key.generation == generation)).count(), 1);
        let (reply, response) = tokio::sync::oneshot::channel();
        actor
            .handle_request(AsyncRuntimeRequest::ClaimAgentCompactionTask {
                pane_id: "%1".into(),
                task_generation: generation,
                reply,
            })
            .await;
        assert!(response.await.unwrap().unwrap().is_none());
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
}

/// Stop before recovery and Stop after an offer retire the task. Releasing
/// capacity and a late actual claim must not resurrect a cancelled generation.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_compaction_source_is_not_recovered() {
    for after_offer in [false, true] {
        let (_root, mut actor, primary, generation) = pending_compaction_at_capacity();
        if after_offer {
            refuse_and_offer(&mut actor, generation);
        } else {
            let effects = actor.pending_provider_dispatch_side_effects().unwrap();
            assert!(actor.admit_committed_side_effects(effects).is_err());
        }
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(claim(&mut actor, generation).await.unwrap().is_none());
        actor.drain_timer_side_effects(64).unwrap();
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert!(!actor.service.agent_is_compacting("%1"));
    }
}

/// An actual human-pause claim returns no work but retains the unclaimed task.
/// A later capacity drain reoffers that same generation once, without granting
/// execution while paused. Stop then retires the exact source normally.
#[tokio::test(flavor = "current_thread")]
async fn paused_compaction_claim_retains_recovery_source() {
    use futures_util::FutureExt;
    let (_root, mut actor, primary, generation) = pending_compaction_with_lifecycle_owner(true);
    let result = std::panic::AssertUnwindSafe(async {
    refuse_and_offer(&mut actor, generation);
    let target = actor
        .service
        .capture_agent_lifecycle_target(&primary, "%1")
        .unwrap();
    actor
        .service
        .pause_agent_lifecycle_target(&primary, &target)
        .unwrap();
    assert!(claim(&mut actor, generation).await.unwrap().is_none());
    let source = SourceDispatch::Compaction("%1".into(), generation);
    assert!(actor.refused_dispatch_sources.contains(&source));
    assert!(!actor.offered_dispatch_sources.contains(&source));
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        matches!(actor.drain_agent_provider_dispatch_side_effects(1).unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == generation)
    );
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .iter().all(|effect| !matches!(effect, RuntimeSideEffect::DispatchAgentCompaction { .. }))
    );
    assert!(actor.offered_dispatch_sources.contains(&source));
    }).catch_unwind().await;
    let stopped = actor.service.execute_agent_shell_command(&primary, "/stop");
    let retired = actor.service.terminate_all_pane_processes();
    if let Err(panic) = result {
        if stopped.is_err() || retired.is_err() {
            eprintln!("compaction pause fixture cleanup remains unproven after assertion failure");
        }
        std::panic::resume_unwind(panic);
    }
    stopped.unwrap();
    retired.unwrap();
}

/// Strict producer refusal never enrolls recovery. A separately accepted new
/// /compact operation likewise cannot inherit an old refused or offered key,
/// even in the same conversation. Both real preparation phases are adopted;
/// a stale actual claim cannot consume the replacement or grant it a lease.
#[tokio::test(flavor = "current_thread")]
async fn strict_or_replaced_compaction_is_not_accelerated() {
    for offered in [None, Some(false), Some(true)] {
        let (_root, mut actor, primary, old) = pending_compaction_at_capacity();
        match offered {
            None => {
                let effects = actor.pending_provider_dispatch_side_effects().unwrap();
                assert!(actor.queue_runtime_side_effects(effects).is_err());
            }
            Some(false) => {
                let effects = actor.pending_provider_dispatch_side_effects().unwrap();
                assert!(actor.admit_committed_side_effects(effects).is_err());
            }
            Some(true) => refuse_and_offer(&mut actor, old),
        }
        let current = if offered.is_some() {
            actor
                .service
                .execute_agent_shell_command(&primary, "/stop")
                .unwrap();
            actor
                .service
                .execute_agent_shell_command(&primary, "/compact")
                .unwrap();
            let work = actor
                .service
                .take_manual_compaction_preparations()
                .remove(0);
            let result = work.execute_source();
            assert!(
                actor
                    .service
                    .complete_manual_compaction_preparation(&work, result)
                    .unwrap()
            );
            let work = actor.service.take_manual_compaction_requests().remove(0);
            let result = work.execute_request();
            assert!(
                actor
                    .service
                    .complete_manual_compaction_request(&work, result)
                    .unwrap()
            );
            let replacement = actor
                .service
                .pending_agent_compaction_task_generation("%1")
                .unwrap();
            assert!(replacement > old);
            assert!(claim(&mut actor, old).await.unwrap().is_none());
            replacement
        } else {
            old
        };
        actor.drain_timer_side_effects(64).unwrap();
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert_eq!(
            actor.service.pending_agent_compaction_task_generation("%1"),
            Some(current)
        );
        assert!(
            matches!(actor.pending_provider_dispatch_side_effects().unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == current)
        );
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
}

/// A recovered offer is not authority to bypass mandatory claim-lease pressure.
/// The actual claim must settle its generation and both metadata sets even when
/// the existing strict lease admission returns early with no executable work.
#[tokio::test(flavor = "current_thread")]
async fn recovered_compaction_lease_refusal_retires_offer() {
    let (_root, mut actor, _primary, generation) = pending_compaction_at_capacity();
    refuse_and_offer(&mut actor, generation);
    assert!(claim(&mut actor, generation).await.unwrap().is_none());
    assert!(
        !actor
            .service
            .agent_compaction_task_is_claimed("%1", generation)
    );
    assert!(
        actor
            .service
            .pending_agent_compaction_task_generation("%1")
            .is_none()
    );
    assert!(!actor.service.agent_is_compacting("%1"));
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
}

/// A real retryable failure replaces a claimed compactor with a new generation
/// under transport backoff. Old refusal metadata cannot accelerate it before
/// its exact retry owner is released, nor enroll ordinary work afterward.
#[tokio::test(flavor = "current_thread")]
async fn recovered_compaction_does_not_accelerate_transport_retry() {
    let (_root, mut actor, primary, generation) = pending_compaction_at_capacity();
    refuse_and_offer(&mut actor, generation);
    actor.drain_timer_side_effects(64).unwrap();
    let _dispatch = claim(&mut actor, generation).await.unwrap().unwrap();
    let transition = actor
        .service
        .apply_agent_compaction_transition(crate::runtime::AgentCompactionEvent::Failed {
            pane_id: "%1".into(),
            task_generation: generation,
            kind: "io".into(),
            message: "provider HTTP response read failed: unexpected EOF".into(),
            usage: Default::default(),
            provider_failure_json: None,
            provider_raw_text: None,
        })
        .unwrap();
    let retry = actor
        .service
        .pending_agent_compaction_task_generation("%1")
        .unwrap();
    assert!(retry > generation);
    assert!(transition.side_effects.iter().any(|effect| matches!(effect, RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::CompactionRetry && key.generation == retry)));
    actor
        .admit_committed_side_effects(transition.side_effects)
        .unwrap();
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .service
            .agent_compaction_retry_delay("%1", retry)
            .is_some()
    );
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(claim(&mut actor, retry).await.unwrap().is_none());
    assert!(actor.service.release_agent_compaction_retry("%1", retry));
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(
        matches!(actor.pending_provider_dispatch_side_effects().unwrap().as_slice(), [RuntimeSideEffect::DispatchAgentCompaction { task_generation, .. }] if *task_generation == retry)
    );
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}
