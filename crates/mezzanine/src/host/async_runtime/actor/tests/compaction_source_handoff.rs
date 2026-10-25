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
