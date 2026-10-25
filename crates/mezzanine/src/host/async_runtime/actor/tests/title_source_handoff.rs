//! Generated-title dispatch custody across finite actor queue pressure.
//!
//! Tasks are admitted through the production conversation scheduler with isolated
//! generated stores and a real configured provider constructor. No inference,
//! native process, timer worker, or persistence worker executes in these tests.

use super::super::*;

/// Keeps generated fixture storage alive until its actor has been destroyed.
struct FixtureRoot(std::path::PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("title handoff fixture storage cleanup failed: {error}");
        }
    }
}

/// Admits an actual optional title request without creating a turn or process,
/// then occupies the entire unchanged finite committed side-effect allowance.
fn pending_title_at_capacity() -> (FixtureRoot, AsyncRuntimeSessionActor, String) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let root = FixtureRoot(
        std::env::temp_dir().join(format!("mez-title-handoff-{:032x}", rand::random::<u128>())),
    );
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_config_root(root.0.clone());
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        root.0.join("transcripts"),
    ));
    service.set_auth_store(crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&root.0),
    ));
    service.replace_config_layers(vec![ConfigLayer {
        name: "title-handoff".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"pane\"\nsession_title_policy=\"generated\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    service
        .attach_primary("title-primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    let conversation = service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap()
        .session_id
        .clone();
    assert_eq!(
        service.schedule_runtime_agent_session_title(
            &conversation,
            Some("Maintain the generated fixture checklist.")
        ),
        Ok(())
    );
    assert_eq!(
        service.pending_agent_session_title_tasks(),
        vec![conversation.clone()]
    );
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
    (root, actor, conversation)
}

/// A refused committed title dispatch retains an admitted service source. Freeing
/// one real slot must offer that task without new prompts, polling or replay.
#[tokio::test(flavor = "current_thread")]
async fn refused_title_recovers_after_real_lane_progress() {
    for compatibility in [false, true] {
        let (_root, mut actor, conversation) = pending_title_at_capacity();
        let dispatch_id = pending_id(&actor, &conversation);
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(
            matches!(effects.as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { conversation_id, .. }] if conversation_id == &conversation),
            "{effects:?}"
        );
        assert!(actor.admit_committed_side_effects(effects).is_err());
        actor.drain_timer_side_effects(1).unwrap();
        let offered = if compatibility {
            actor.drain_runtime_side_effects(1).unwrap()
        } else {
            actor.drain_agent_provider_dispatch_side_effects(1).unwrap()
        };
        assert!(
            matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { conversation_id, .. }] if conversation_id == &conversation),
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
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        let dispatch = claim(&mut actor, &conversation, &dispatch_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dispatch.task.dispatch_id, dispatch_id);
        assert!(!dispatch.task.attempt_id.is_empty());
        assert_ne!(dispatch.task.dispatch_id, dispatch.task.attempt_id);
        assert!(
            claim(&mut actor, &conversation, &dispatch_id)
                .await
                .unwrap()
                .is_none()
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        actor.service.cancel_agent_session_title_task(&conversation);
    }
}

/// Captures only the immutable identity of the fixture's actual pending task.
fn pending_id(actor: &AsyncRuntimeSessionActor, conversation: &str) -> String {
    actor
        .service
        .pending_agent_session_title_dispatch_id(conversation)
        .unwrap()
        .to_string()
}

/// Sends the worker's exact descriptor through the real actor/service boundary.
async fn claim(
    actor: &mut AsyncRuntimeSessionActor,
    conversation: &str,
    dispatch: &str,
) -> Result<Option<crate::runtime::RuntimeAgentSessionTitleDispatch>> {
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimAgentSessionTitleTask {
            conversation_id: conversation.to_string(),
            dispatch_id: dispatch.to_string(),
            reply,
        })
        .await;
    response.await.unwrap()
}

/// Cancellation before or after offer removes the service source. Real drains
/// and delayed actor claims must not resurrect a cancelled auxiliary request.
#[tokio::test(flavor = "current_thread")]
async fn cancelled_title_is_not_recovered_or_claimed() {
    for after_offer in [false, true] {
        let (_root, mut actor, conversation) = pending_title_at_capacity();
        let dispatch = pending_id(&actor, &conversation);
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(actor.admit_committed_side_effects(effects).is_err());
        if after_offer {
            actor.drain_timer_side_effects(1).unwrap();
            assert_eq!(
                actor
                    .drain_agent_provider_dispatch_side_effects(1)
                    .unwrap()
                    .len(),
                1
            );
        }
        assert!(actor.service.cancel_agent_session_title_task(&conversation));
        assert!(
            claim(&mut actor, &conversation, &dispatch)
                .await
                .unwrap()
                .is_none()
        );
        actor.drain_timer_side_effects(64).unwrap();
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert!(
            !actor
                .service
                .agent_session_title_task_is_scheduled(&conversation)
        );
    }
}

/// Producer refusal is still strict and does not enrol its pending title into
/// committed-source recovery. Only ordinary scheduling may next offer that work.
#[test]
fn strict_title_refusal_does_not_enrol_recovery() {
    let (_root, mut actor, conversation) = pending_title_at_capacity();
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.queue_runtime_side_effects(effects).is_err());
    actor.drain_timer_side_effects(64).unwrap();
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .len(),
        1
    );
    actor.service.cancel_agent_session_title_task(&conversation);
}

/// A genuine cancellation followed by ordinary scheduling creates a fresh
/// admission in the same conversation. Claiming the old offered descriptor BEFORE
/// metadata pruning cannot consume or hide the new task, even with an old record
/// still queued. Its newly projected exact descriptor remains independently usable.
#[tokio::test(flavor = "current_thread")]
async fn replacement_title_is_not_consumed_or_hidden_by_old_dispatch() {
    for drain_old in [false, true] {
        let (_root, mut actor, conversation) = pending_title_at_capacity();
        let original = pending_id(&actor, &conversation);
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(actor.admit_committed_side_effects(effects).is_err());
        actor.drain_timer_side_effects(1).unwrap();
        if drain_old {
            assert_eq!(
                actor
                    .drain_agent_provider_dispatch_side_effects(1)
                    .unwrap()
                    .len(),
                1
            );
        }
        assert!(actor.service.cancel_agent_session_title_task(&conversation));
        assert_eq!(
            actor.service.schedule_runtime_agent_session_title(
                &conversation,
                Some("Maintain the replacement fixture checklist.")
            ),
            Ok(())
        );
        let replacement = pending_id(&actor, &conversation);
        assert_ne!(original, replacement);
        assert!(
            claim(&mut actor, &conversation, &original)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(pending_id(&actor, &conversation), replacement);
        let effects = actor.pending_provider_dispatch_side_effects().unwrap();
        assert!(
            matches!(effects.as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { dispatch_id, .. }] if dispatch_id == &replacement)
        );
        actor.drain_timer_side_effects(64).unwrap();
        // Drain only stale descriptors; the replacement has not been enrolled.
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap();
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        actor.queue_runtime_side_effects(effects).unwrap();
        assert_eq!(
            actor
                .drain_agent_provider_dispatch_side_effects(1)
                .unwrap()
                .len(),
            1
        );
        let current = claim(&mut actor, &conversation, &replacement)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(current.task.dispatch_id, replacement);
        actor.service.cancel_agent_session_title_task(&conversation);
    }
}

/// Actual claim-lease expiry schedules the controller's one permitted retry.
/// The predecessor's descriptor cannot claim that retry; the retry still needs
/// its own committed refusal/recovery and gets a distinct issued accounting ID.
/// A synthetic late original outcome charges once without settling the new claim.
#[tokio::test(flavor = "current_thread")]
async fn title_retry_has_distinct_dispatch_and_late_accounting_owner() {
    let (_root, mut actor, conversation) = pending_title_at_capacity();
    let original = pending_id(&actor, &conversation);
    let first = claim(&mut actor, &conversation, &original)
        .await
        .unwrap()
        .unwrap();
    let retry_effects = actor
        .service
        .reap_expired_agent_session_title_claims(u64::MAX)
        .unwrap();
    let retry = pending_id(&actor, &conversation);
    assert_ne!(original, retry);
    assert!(
        claim(&mut actor, &conversation, &original)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(pending_id(&actor, &conversation), retry);
    assert!(actor.admit_committed_side_effects(retry_effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { dispatch_id, .. }] if dispatch_id == &retry)
    );
    let second = claim(&mut actor, &conversation, &retry)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.task.dispatch_id, retry);
    assert_ne!(second.task.attempt_id, first.task.attempt_id);
    let event = crate::runtime::AgentSessionTitleEvent::WorkerSettled {
        conversation_id: conversation.clone(),
        attempt_id: first.task.attempt_id,
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 23,
            output_tokens: 2,
            ..Default::default()
        },
        outcome: crate::runtime::AgentSessionTitleOutcome::Generated("Stale fixture title".into()),
    };
    for _ in 0..2 {
        assert!(
            !actor
                .service
                .apply_agent_session_title_transition(event.clone())
                .unwrap()
                .applied
        );
    }
    assert_eq!(
        actor.service.claimed_agent_session_title_task_ids(),
        vec![conversation.clone()]
    );
    let usage = actor
        .service
        .agent_token_usage_for_conversation(&conversation);
    assert_eq!(
        usage.values().map(|usage| usage.input_tokens).sum::<u64>(),
        23
    );
    assert_eq!(
        usage.values().map(|usage| usage.output_tokens).sum::<u64>(),
        2
    );
    // The second lease reaches the existing attempt limit, not another retry.
    assert!(
        actor
            .service
            .reap_expired_agent_session_title_claims(u64::MAX)
            .unwrap()
            .is_empty()
    );
    assert!(
        !actor
            .service
            .agent_session_title_task_is_scheduled(&conversation)
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
}

/// Fill the real unchanged issued-owner bound through 256 ordinary admissions
/// and actual claims, without model I/O or synthetic inserted tasks. The next
/// claim returns no work and restores exactly its pending dispatch. A late old
/// result releases one slot; real lane progress must reoffer and claim it once.
#[tokio::test(flavor = "current_thread")]
async fn title_accounting_pressure_restores_same_pending_dispatch() {
    let (_root, mut actor, conversation) = pending_title_at_capacity();
    let mut first = None;
    for index in 0..256 {
        let dispatch_id = pending_id(&actor, &conversation);
        let dispatch = claim(&mut actor, &conversation, &dispatch_id)
            .await
            .unwrap()
            .unwrap();
        if index == 0 {
            first = Some(dispatch.task.attempt_id);
        }
        assert!(actor.service.cancel_agent_session_title_task(&conversation));
        assert_eq!(
            actor.service.schedule_runtime_agent_session_title(
                &conversation,
                Some("Maintain the accounting fixture checklist.")
            ),
            Ok(())
        );
    }
    let dispatch_id = pending_id(&actor, &conversation);
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    assert!(
        claim(&mut actor, &conversation, &dispatch_id)
            .await
            .unwrap()
            .is_none()
    );
    let source = SourceDispatch::Title(conversation.clone(), dispatch_id.clone());
    assert!(actor.refused_dispatch_sources.contains(&source));
    assert!(!actor.offered_dispatch_sources.contains(&source));
    assert_eq!(pending_id(&actor, &conversation), dispatch_id);
    assert!(
        actor
            .service
            .claimed_agent_session_title_task_ids()
            .is_empty()
    );
    assert!(
        !actor
            .service
            .apply_agent_session_title_transition(
                crate::runtime::AgentSessionTitleEvent::WorkerSettled {
                    conversation_id: conversation.clone(),
                    attempt_id: first.unwrap(),
                    usage: Default::default(),
                    outcome: crate::runtime::AgentSessionTitleOutcome::Rejected("timeout".into()),
                }
            )
            .unwrap()
            .applied
    );
    actor.drain_timer_side_effects(1).unwrap();
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentSessionTitle { dispatch_id: id, .. }] if id == &dispatch_id)
    );
    let dispatch = claim(&mut actor, &conversation, &dispatch_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dispatch.task.dispatch_id, dispatch_id);
    assert!(!dispatch.task.attempt_id.is_empty());
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    actor.service.cancel_agent_session_title_task(&conversation);
}

/// Removing the configured provider after a real title admission faults the
/// actual constructor. Its consumed source and recovered offer must retire; lane
/// progress cannot regenerate a doomed task or claim that a request was issued.
#[tokio::test(flavor = "current_thread")]
async fn title_constructor_failure_retires_source_and_offer() {
    let (_root, mut actor, conversation) = pending_title_at_capacity();
    let dispatch = pending_id(&actor, &conversation);
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    actor.service.replace_config_layers(Vec::new()).unwrap();
    assert_eq!(pending_id(&actor, &conversation), dispatch);
    let error = claim(&mut actor, &conversation, &dispatch)
        .await
        .unwrap_err();
    assert!(error.message().contains("provider"), "{error}");
    assert!(
        !actor
            .service
            .agent_session_title_task_is_scheduled(&conversation)
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
        claim(&mut actor, &conversation, &dispatch)
            .await
            .unwrap()
            .is_none()
    );
}

/// Rebinding the pane leaves no eligible original source. A previously offered
/// title descriptor cannot spend a provider request against its retired binding.
#[tokio::test(flavor = "current_thread")]
async fn rebound_title_has_no_projection_or_claim() {
    let (_root, mut actor, conversation) = pending_title_at_capacity();
    let dispatch = pending_id(&actor, &conversation);
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    actor
        .service
        .agent_shell_store_mut()
        .bind_conversation("%1", "replacement-conversation", 0)
        .unwrap();
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    assert!(
        claim(&mut actor, &conversation, &dispatch)
            .await
            .unwrap()
            .is_none()
    );
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    actor.service.cancel_agent_session_title_task(&conversation);
}
