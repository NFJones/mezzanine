//! Durable-memory dispatch custody across finite actor queue pressure.
//!
//! These fixtures admit actual `/remember` commands with generated source and
//! isolated stores. They exercise lane transfer and actor/service claim owners,
//! never provider inference, native children, or persistence workers.

use super::super::*;

/// Owns generated fixture storage through actor destruction, including unwind.
struct FixtureRoot(std::path::PathBuf);

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("remember handoff fixture storage cleanup failed: {error}");
        }
    }
}

/// Admits real queued memory generation before actor construction, drains only
/// startup descriptors, then fills the existing finite committed allowance.
fn pending_remember_at_capacity() -> (FixtureRoot, AsyncRuntimeSessionActor, ClientId) {
    pending_remember_with_auth(true)
}

/// Optional auth-store attachment faults real provider construction after
/// successful command admission; no credentials or fake task are injected.
fn pending_remember_with_auth(auth: bool) -> (FixtureRoot, AsyncRuntimeSessionActor, ClientId) {
    use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
    let root = FixtureRoot(std::env::temp_dir().join(format!(
        "mez-remember-handoff-{:032x}",
        rand::random::<u128>()
    )));
    let mut service = crate::test_support::runtime::RuntimeServiceFixture::new().build();
    service.set_config_root(root.0.clone());
    service.set_agent_transcript_store(crate::storage::transcript::AgentTranscriptStore::new(
        root.0.join("transcripts"),
    ));
    if auth {
        service.set_auth_store(crate::security::auth::AuthStore::new(
            crate::security::auth::AuthPaths::under_config_root(&root.0),
        ));
    }
    service.replace_config_layers(vec![ConfigLayer {
        name: "remember-handoff".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"default\"\nshell_mode=\"pane\"\n[providers.runtime-batch]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:9/v1\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.default]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=128000\n[permissions]\nsandbox=\"policy-only\"\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("remember-primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let admitted = service
        .execute_agent_shell_command(
            &primary,
            "/remember Keep the generated fixture checklist current.",
        )
        .unwrap();
    assert!(service.agent_is_remembering("%1"), "{admitted}");
    assert_eq!(service.pending_agent_remember_tasks(), vec!["%1"]);
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
    (root, actor, primary)
}

/// Issues the actual actor claim, carrying only the identity in the descriptor.
async fn claim(
    actor: &mut AsyncRuntimeSessionActor,
    observation_id: &str,
) -> Result<Option<crate::runtime::RuntimeAgentRememberDispatch>> {
    let (reply, response) = tokio::sync::oneshot::channel();
    actor
        .handle_request(AsyncRuntimeRequest::ClaimAgentRememberTask {
            pane_id: "%1".into(),
            observation_id: observation_id.to_string(),
            reply,
        })
        .await;
    response.await.unwrap()
}

/// Captures the current exact source through ordinary service projection.
fn pending(actor: &AsyncRuntimeSessionActor) -> (Vec<RuntimeSideEffect>, String) {
    let effects = actor.pending_provider_dispatch_side_effects().unwrap();
    let [
        RuntimeSideEffect::DispatchAgentRemember {
            pane_id,
            observation_id,
        },
    ] = effects.as_slice()
    else {
        panic!("expected exactly one real remember descriptor: {effects:?}");
    };
    assert_eq!(pane_id, "%1");
    let observation = observation_id.clone();
    (effects, observation)
}

/// A real refused committed remember descriptor must recover after lane progress
/// without a new input/event. Both drain adapters must fence its pending source
/// until the actor performs the actual claim, not merely until adapter drain.
#[tokio::test(flavor = "current_thread")]
async fn refused_remember_recovers_and_claims_once() {
    for compatibility in [false, true] {
        let (_root, mut actor, primary) = pending_remember_at_capacity();
        let (effects, observation) = pending(&actor);
        assert!(actor.admit_committed_side_effects(effects).is_err());
        actor.drain_timer_side_effects(1).unwrap();
        let offered = if compatibility {
            actor.drain_runtime_side_effects(1).unwrap()
        } else {
            actor.drain_agent_provider_dispatch_side_effects(1).unwrap()
        };
        assert!(
            matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentRemember { pane_id, observation_id }] if pane_id == "%1" && observation_id == &observation),
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
        let dispatch = claim(&mut actor, &observation).await.unwrap().unwrap();
        assert_eq!(dispatch.task.pane_id, "%1");
        assert_eq!(dispatch.task.observation_id, observation);
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert!(actor.service.pending_agent_remember_tasks().is_empty());
        assert!(claim(&mut actor, &observation).await.unwrap().is_none());
        let stopped = actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(stopped.contains("remember_cancelled=true"), "{stopped}");
    }
}

/// Stop retires a genuinely refused source both before and after worker offer.
/// Late actor claims remain no-work, and real lane progress cannot resurrect it.
#[tokio::test(flavor = "current_thread")]
async fn stopped_remember_is_not_recovered_or_claimed() {
    for after_offer in [false, true] {
        let (_root, mut actor, primary) = pending_remember_at_capacity();
        let (effects, observation) = pending(&actor);
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
            assert_eq!(actor.offered_dispatch_sources.len(), 1);
        }
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        actor.drain_timer_side_effects(64).unwrap();
        assert!(claim(&mut actor, &observation).await.unwrap().is_none());
        assert!(
            actor
                .drain_agent_provider_dispatch_side_effects(64)
                .unwrap()
                .is_empty()
        );
        assert!(actor.refused_dispatch_sources.is_empty());
        assert!(actor.offered_dispatch_sources.is_empty());
        assert!(!actor.service.agent_is_remembering("%1"));
    }
}

/// Ordinary strict producer refusal does not enroll source recovery. The task
/// remains eligible through normal projection, without automatic drain refill.
#[tokio::test(flavor = "current_thread")]
async fn strict_remember_refusal_is_not_accelerated() {
    let (_root, mut actor, primary) = pending_remember_at_capacity();
    let (effects, observation) = pending(&actor);
    assert!(actor.queue_runtime_side_effects(effects).is_err());
    assert!(actor.refused_dispatch_sources.is_empty());
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert_eq!(pending(&actor).1, observation);
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// A real new command after Stop has a new observation even in the same pane
/// and conversation. Neither refused nor offered old custody can accelerate,
/// hide, or claim the new task. It still claims normally from its own descriptor.
#[tokio::test(flavor = "current_thread")]
async fn replaced_remember_requires_its_own_observation() {
    for after_offer in [false, true] {
        let (_root, mut actor, primary) = pending_remember_at_capacity();
        let (effects, old) = pending(&actor);
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
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        actor
            .service
            .execute_agent_shell_command(
                &primary,
                "/remember Keep the replacement fixture checklist current.",
            )
            .unwrap();
        let (_, replacement) = pending(&actor);
        assert_ne!(replacement, old);
        // A stale issued descriptor must not consume the replacement, even
        // before a drain has pruned old refusal/offer metadata.
        assert!(claim(&mut actor, &old).await.unwrap().is_none());
        assert_eq!(pending(&actor).1, replacement);
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
            actor
                .queue_pending_provider_dispatch_side_effects()
                .unwrap(),
            1
        );
        let descriptor = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
        assert!(
            matches!(descriptor.as_slice(), [RuntimeSideEffect::DispatchAgentRemember { observation_id, .. }] if observation_id == &replacement)
        );
        let dispatch = claim(&mut actor, &replacement).await.unwrap().unwrap();
        assert_eq!(dispatch.task.observation_id, replacement);
        actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
}

/// Actual provider construction fails without an attached auth store after the
/// recovered task is consumed. Claim error must retire custody and active state,
/// not leave a phantom offer or reissue the failed auxiliary model request.
#[tokio::test(flavor = "current_thread")]
async fn remember_constructor_failure_retires_recovered_offer() {
    let (_root, mut actor, _primary) = pending_remember_with_auth(false);
    let (effects, observation) = pending(&actor);
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    let error = claim(&mut actor, &observation).await.unwrap_err();
    assert!(
        error.message().contains("attached provider auth store"),
        "{error}"
    );
    assert!(!actor.service.agent_is_remembering("%1"));
    assert!(actor.service.pending_agent_remember_tasks().is_empty());
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

/// Conversation ownership is independent of immutable observation identity. A
/// changed binding cannot authorize an old queued request; old custody is pruned
/// without taking or executing that task, then normal Stop retires its source.
#[tokio::test(flavor = "current_thread")]
async fn rebound_remember_has_no_dispatch_or_claim_authority() {
    let (_root, mut actor, primary) = pending_remember_at_capacity();
    let (effects, observation) = pending(&actor);
    assert!(actor.admit_committed_side_effects(effects).is_err());
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
    assert!(claim(&mut actor, &observation).await.unwrap().is_none());
    actor.drain_timer_side_effects(64).unwrap();
    assert!(
        actor
            .drain_agent_provider_dispatch_side_effects(64)
            .unwrap()
            .is_empty()
    );
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(actor.service.pending_agent_remember_tasks().is_empty());
}

/// Real issued accounting owners fill the existing 256-slot bound. A recovered
/// claim refusal must return the still-pending exact source to recovery rather
/// than leave a phantom worker offer. One genuine late result releases a slot;
/// real lane progress then reoffers and claims that same observation once.
#[tokio::test(flavor = "current_thread")]
async fn remember_accounting_pressure_preserves_unclaimed_recovery() {
    let (_root, mut actor, primary) = pending_remember_at_capacity();
    let mut first = None;
    for index in 0..256 {
        let (_, observation) = pending(&actor);
        let dispatch = claim(&mut actor, &observation).await.unwrap().unwrap();
        if index == 0 {
            first = Some(dispatch.task.observation_id);
        }
        let stopped = actor
            .service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        assert!(stopped.contains("remember_cancelled=true"), "{stopped}");
        actor
            .service
            .execute_agent_shell_command(
                &primary,
                "/remember Keep the bounded accounting fixture current.",
            )
            .unwrap();
    }
    let (effects, observation) = pending(&actor);
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    let error = claim(&mut actor, &observation).await.unwrap_err();
    assert!(
        error
            .message()
            .contains("auxiliary accounting capacity exhausted"),
        "{error}"
    );
    let source = SourceDispatch::Remember("%1".into(), observation.clone());
    assert!(actor.refused_dispatch_sources.contains(&source));
    assert!(!actor.offered_dispatch_sources.contains(&source));
    assert_eq!(pending(&actor).1, observation);
    let transition = actor
        .service
        .apply_agent_remember_transition(crate::runtime::AgentRememberEvent::Failed {
            pane_id: "%1".into(),
            observation_id: first.unwrap(),
            usage: mez_agent::ModelTokenUsage::default(),
            kind: "io".into(),
            message: "generated fixture outcome".into(),
            provider_failure_json: None,
            provider_raw_text: None,
        })
        .unwrap();
    assert!(!transition.applied);
    actor.drain_timer_side_effects(1).unwrap();
    let offered = actor.drain_agent_provider_dispatch_side_effects(1).unwrap();
    assert!(
        matches!(offered.as_slice(), [RuntimeSideEffect::DispatchAgentRemember { observation_id, .. }] if observation_id == &observation)
    );
    assert!(
        actor
            .pending_provider_dispatch_side_effects()
            .unwrap()
            .is_empty()
    );
    let dispatch = claim(&mut actor, &observation).await.unwrap().unwrap();
    assert_eq!(dispatch.task.observation_id, observation);
    assert!(actor.refused_dispatch_sources.is_empty());
    assert!(actor.offered_dispatch_sources.is_empty());
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}

/// Slash Stop after an actual recovered claim revokes publication, not incurred
/// accounting. Replaying the original synthetic late failure charges its frozen
/// observation once and cannot cancel a genuinely claimed replacement request.
#[tokio::test(flavor = "current_thread")]
async fn stopped_remember_late_result_preserves_expense_not_publication() {
    let (_root, mut actor, primary) = pending_remember_at_capacity();
    let (effects, original) = pending(&actor);
    assert!(actor.admit_committed_side_effects(effects).is_err());
    actor.drain_timer_side_effects(1).unwrap();
    assert_eq!(
        actor
            .drain_agent_provider_dispatch_side_effects(1)
            .unwrap()
            .len(),
        1
    );
    let dispatch = claim(&mut actor, &original).await.unwrap().unwrap();
    let conversation = dispatch.task.conversation_id;
    let stopped = actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
    assert!(stopped.contains("remember_cancelled=true"), "{stopped}");
    assert!(
        !actor
            .service
            .agent_remember_observation_matches("%1", &original)
    );
    actor
        .service
        .execute_agent_shell_command(
            &primary,
            "/remember Preserve only the replacement fixture request.",
        )
        .unwrap();
    let (_, replacement) = pending(&actor);
    let current = claim(&mut actor, &replacement).await.unwrap().unwrap();
    assert_ne!(current.task.observation_id, original);
    let event = crate::runtime::AgentRememberEvent::Failed {
        pane_id: "%1".into(),
        observation_id: original,
        usage: mez_agent::ModelTokenUsage {
            input_tokens: 23,
            output_tokens: 2,
            ..Default::default()
        },
        kind: "io".into(),
        message: "generated fixture outcome".into(),
        provider_failure_json: None,
        provider_raw_text: None,
    };
    for _ in 0..2 {
        assert!(
            !actor
                .service
                .apply_agent_remember_transition(event.clone())
                .unwrap()
                .applied
        );
        assert!(
            actor
                .service
                .agent_remember_observation_matches("%1", &replacement)
        );
        assert!(actor.service.agent_is_remembering("%1"));
    }
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
    actor
        .service
        .execute_agent_shell_command(&primary, "/stop")
        .unwrap();
}
