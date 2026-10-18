//! Admission boundary for worker claims that require a bounded timer lease.
//!
//! Claim state is recorded on the actor before admission, and worker dispatch
//! remains withheld until this operation succeeds. A rejected admission retires
//! only the exact undispatched owner. Provider and compaction settlement retain
//! their separate service policies; no queued unrelated work is drained here.

use super::{
    AgentId, AsyncRuntimeSessionActor, MezError, Result, RuntimeSideEffect, RuntimeTimerKey,
    RuntimeTimerKind,
};

/// Exact owner of an undispatched provider or compaction worker claim.
pub(super) enum WorkerClaimLease<'a> {
    /// Provider turn and generation validated before dispatch leaves the actor.
    Provider {
        agent_id: &'a AgentId,
        turn_id: &'a str,
        generation: u64,
    },
    /// Compaction task whose pane may retain independently leased generations.
    Compaction { pane_id: &'a str, generation: u64 },
    /// Frozen pending task whose next dispatch requires supervised backoff.
    CompactionRetry { pane_id: &'a str, generation: u64 },
}

impl WorkerClaimLease<'_> {
    /// Returns the mandatory timer identity for this exact claim.
    fn key(&self) -> RuntimeTimerKey {
        match self {
            Self::Provider {
                turn_id,
                generation,
                ..
            } => RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, *turn_id, *generation),
            Self::Compaction {
                pane_id,
                generation,
            } => RuntimeTimerKey::new(RuntimeTimerKind::CompactionClaim, *pane_id, *generation),
            Self::CompactionRetry {
                pane_id,
                generation,
            } => RuntimeTimerKey::new(RuntimeTimerKind::CompactionRetry, *pane_id, *generation),
        }
    }

    /// Checks claim ownership without assuming the pane id alone is authority.
    fn is_current(&self, actor: &AsyncRuntimeSessionActor) -> bool {
        match self {
            Self::Provider {
                agent_id,
                turn_id,
                generation,
            } => actor
                .service
                .agent_provider_claim_matches(agent_id, turn_id, *generation),
            Self::Compaction {
                pane_id,
                generation,
            } => actor
                .service
                .agent_compaction_task_is_claimed(pane_id, *generation),
            Self::CompactionRetry {
                pane_id,
                generation,
            } => actor
                .service
                .agent_compaction_retry_delay(pane_id, *generation)
                .is_some(),
        }
    }
}

impl AsyncRuntimeSessionActor {
    /// Admits the mandatory lease before returning work to an external worker.
    /// Returns false after rejecting stale work or settling failed admission;
    /// settlement errors remain visible instead of orphaning an unleased claim.
    pub(super) fn admit_worker_claim_lease(
        &mut self,
        owner: WorkerClaimLease<'_>,
        effects: Vec<RuntimeSideEffect>,
    ) -> Result<bool> {
        if !owner.is_current(self) {
            return Ok(false);
        }
        let key = owner.key();
        let valid = effects.iter().filter(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key: candidate, delay_ms } if candidate == &key && *delay_ms > 0
        )).count() == 1;
        let admitted = if valid {
            self.queue_runtime_side_effects(effects)
        } else {
            Err(MezError::invalid_state(
                "worker claim admission requires exactly one matching bounded lease",
            ))
        };
        let Err(error) = admitted else {
            return Ok(true);
        };
        #[cfg(test)]
        if let WorkerClaimLease::Provider {
            turn_id,
            generation,
            ..
        } = &owner
        {
            self.service
                .observe_provider_lease_refusal_for_tests(turn_id, *generation, &error);
        }
        // No await or worker handoff occurs between recording and this guard.
        // Still check identity explicitly so a future caller cannot settle a replacement.
        if owner.is_current(self) {
            match owner {
                WorkerClaimLease::Provider { turn_id, .. } => self
                    .service
                    .fail_configured_agent_provider_task(turn_id, &error)?,
                WorkerClaimLease::Compaction {
                    pane_id,
                    generation,
                } => {
                    self.service.expire_claimed_agent_compaction_task(pane_id, generation)
                        .map_err(|settlement_error| MezError::invalid_state(format!(
                            "compaction claim timer admission failed: {}; settlement failed: {}", error.message(), settlement_error.message()
                        )))?;
                }
                WorkerClaimLease::CompactionRetry {
                    pane_id,
                    generation,
                } => {
                    self.service
                        .fail_agent_compaction_retry_admission(pane_id, generation)?;
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::async_runtime::AsyncRuntimeActorConfig;
    use crate::test_support::runtime::RuntimeServiceFixture;
    use mez_mux::layout::Size;

    /// A real completed RuntimeOnly G1 must survive refusal of its ordinary
    /// provider continuation's mandatory timer. Setup admits trusted synthetic
    /// provider output and two serialized external outcomes through production
    /// APIs; it never forges a claim/group or executes a provider, HTTP/MCP,
    /// native command, sandbox probe or assessment. The actual actor request
    /// handles preparation, provisional accounting and capacity refusal. Inspect
    /// custody BEFORE shutdown; a missing G1 is the desired baseline failure,
    /// distinct from all readiness, preparation and refusal precondition checks.
    #[tokio::test(flavor = "current_thread")]
    async fn provider_lease_refusal_retains_original_runtime_only_completion() {
        use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
        use crate::host::async_runtime::actor::queue::SIDE_EFFECT_QUEUE_FULL_PREFIX;
        use crate::runtime::{RuntimeApprovedExternalActionOutcome, RuntimeSessionService};
        use mez_agent::{
            ActionResult, AgentAction, AgentActionPayload, AgentTurnExecution, AgentTurnState,
        };

        let mut service = RuntimeServiceFixture::new().build();
        let auth_root = std::env::temp_dir().join(format!(
            "mez-runtime-only-lease-refusal-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        assert!(
            !auth_root.exists(),
            "setup requires an isolated absent credential root"
        );
        service.set_auth_store(crate::security::auth::AuthStore::new(
            crate::security::auth::AuthPaths::under_config_root(&auth_root),
        ));
        service.replace_config_layers(vec![ConfigLayer {
            name: "runtime-only-lease-refusal".into(), path: None,
            format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
            text: "[agents]\ndefault_provider = \"runtime-batch\"\ndefault_model_profile = \"default\"\nshell_mode = \"pane\"\n[providers.runtime-batch]\nkind = \"openai\"\nmodels = [\"test\"]\ndefault_model = \"test\"\n[model_profiles.default]\nprovider = \"runtime-batch\"\nmodel = \"test\"\n".into(),
        }]).unwrap();
        let primary = service
            .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        let started = service
            .start_agent_prompt_turn("%1", "retain original RuntimeOnly results")
            .unwrap();
        let turn = service
            .agent_turn_ledger()
            .turn(&started.turn_id)
            .unwrap()
            .clone();
        let agent = AgentId::opaque(turn.agent_id.clone()).unwrap();
        service.remove_pending_agent_provider_task(&turn.turn_id);
        let actions: Vec<_> = ["first", "second"]
            .into_iter()
            .map(|id| AgentAction {
                id: id.into(),
                payload: AgentActionPayload::FetchUrl {
                    url: format!("https://example.test/runtime-only-{id}"),
                    format: None,
                    max_bytes: None,
                },
            })
            .collect();
        let execution = AgentTurnExecution {
            request: mez_agent::ModelRequest {
                provider: "runtime-batch".into(),
                model: "test".into(),
                model_capabilities: Default::default(),
                max_input_tokens: None,
                reasoning_effort: None,
                thinking_enabled: None,
                latency_preference: None,
                prompt_cache_retention: None,
                max_output_tokens: None,
                temperature: None,
                stop: None,
                prompt_cache_session_id: None,
                prompt_cache_lineage_id: None,
                turn_id: turn.turn_id.clone(),
                agent_id: turn.agent_id.clone(),
                available_mcp_tools: Vec::new(),
                memory_actions_enabled: false,
                issue_actions_enabled: true,
                interaction_kind: mez_agent::ModelInteractionKind::ActionExecution,
                allowed_actions: mez_agent::AllowedActionSet::capability_decision(),
                messages: vec![mez_agent::ModelMessage {
                    role: mez_agent::ModelMessageRole::User,
                    source: mez_agent::ContextSourceKind::UserInstruction,
                    placement: mez_agent::ContextPlacement::ConversationAppend,
                    content: "retain original RuntimeOnly results".into(),
                }]
                .into(),
            },
            response: mez_agent::ModelResponse {
                provider: "runtime-batch".into(),
                model: "test".into(),
                raw_text: "synthetic completed-group source".into(),
                usage: Default::default(),
                latest_request_usage: None,
                quota_usage: Default::default(),
                action_batch: Some(mez_agent::MaapBatch {
                    rationale: "inert ordinary continuation custody".into(),
                    actions: actions.clone(),
                }),
                provider_transcript_events: Vec::new(),
            },
            latest_response_usage: Default::default(),
            routing_token_usage_by_model: Default::default(),
            action_results: actions
                .iter()
                .map(|action| {
                    ActionResult::running(
                        &turn,
                        action,
                        vec!["awaiting synthetic external completion".into()],
                        None,
                    )
                })
                .collect(),
            final_turn: false,
            terminal_state: AgentTurnState::Running,
        };
        service
            .apply_agent_provider_completed_event(&agent, &turn.turn_id, execution)
            .await
            .unwrap();
        let mut expected_results = Vec::new();
        for action in &actions {
            let dispatch = service
                .claim_approved_external_action(&turn.turn_id, &action.id)
                .unwrap()
                .expect("fixture must claim its real serialized external attempt");
            let result = ActionResult::succeeded(
                &turn,
                action,
                vec![format!("F668_RUNTIME_ONLY_ORIGINAL_{}", action.id)],
                None,
            );
            expected_results.push(result.clone());
            assert!(
                service
                    .complete_approved_external_action(RuntimeApprovedExternalActionOutcome {
                        turn_id: turn.turn_id.clone(),
                        action_id: action.id.clone(),
                        attempt: dispatch.attempt,
                        result: Ok(result),
                        mcp_transport: None,
                    })
                    .unwrap()
            );
        }
        service.retry_completion_publications();
        assert!(
            service.agent_provider_task_is_pending(&turn.turn_id),
            "setup: completed G1 must queue ordinary continuation"
        );
        assert!(
            service
                .ready_agent_provider_turn(&agent, &turn.turn_id)
                .unwrap()
                .is_some(),
            "setup: continuation must be genuinely ready"
        );
        let group = service.runtime_only_completion_group_for_tests(&turn.turn_id)
            .expect("setup: actual admission must select RuntimeOnly, not an original store or ephemeral policy");
        let (original_execution, original_context, original_error) = service
            .original_completion_source_for_tests(&turn.turn_id, &group)
            .expect("setup: exact G1 result AND context must exist before claiming");
        assert_eq!(original_execution.action_results, expected_results);
        assert!(original_error.is_none());
        assert!(original_context.chronology().iter().any(|event| {
            event
                .execution_group_id()
                .is_some_and(|owner| owner.as_str() == group)
                && event
                    .block()
                    .content
                    .contains("F668_RUNTIME_ONLY_ORIGINAL_")
        }));

        let mut actor = AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default())
            .unwrap()
            .1;
        // Execute the SAME preparation request and worker-side preparation as
        // the public handle, without starting a provider or the actor run loop.
        let (reply, receive) = tokio::sync::oneshot::channel();
        actor
            .handle_request(
                super::super::AsyncRuntimeRequest::PrepareConfiguredAgentProviderTask {
                    agent_id: agent.clone(),
                    turn_id: turn.turn_id.clone(),
                    reply,
                },
            )
            .await;
        let work = receive
            .await
            .unwrap()
            .unwrap()
            .expect("setup: real preparation request must return work");
        assert!(
            work.mcp_plans.is_empty(),
            "setup must not execute MCP discovery"
        );
        assert_eq!(
            work.provider_auth_refresh_profiles,
            vec!["runtime-batch".to_string()]
        );
        // Actual preparation retains its selected identity. Absent metadata
        // returns Ok(false) at auth/store.rs before secret lookup or refresh
        // transport; do not clear profiles or manufacture a preparation result.
        assert!(
            work.auth_store
                .as_ref()
                .expect("setup auth store")
                .read_metadata_for_provider("runtime-batch")
                .unwrap()
                .is_none()
        );
        assert!(!auth_root.exists());
        assert!(
            work.accounting_projects.is_none(),
            "setup must not run an accounting preparation worker"
        );
        let preparation = RuntimeSessionService::execute_agent_provider_preparation(work).await;
        assert!(
            preparation.provider_refresh_error.is_none(),
            "setup: preparation failure is NOT the custody regression"
        );
        assert!(preparation.mcp.is_empty());
        assert!(preparation.accounting_projects.is_none());
        assert!(
            !auth_root.exists(),
            "inert preparation must not create credential state"
        );
        // Drain setup effects through the normal adapter API without executing
        // them. No worker has received a dispatch and no claim timer may exist.
        let (reply, receive) = tokio::sync::oneshot::channel();
        actor
            .handle_request(super::super::AsyncRuntimeRequest::DrainRuntimeSideEffects {
                limit: 1024,
                reply,
            })
            .await;
        let setup_effects = receive.await.unwrap().unwrap();
        assert!(!setup_effects.iter().any(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::ProviderClaim)));
        assert!(actor.service.agent_provider_task_is_pending(&turn.turn_id));
        assert!(!actor.service.agent_provider_task_is_claimed(&turn.turn_id));
        actor.side_effect_buffer = 1;
        actor
            .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: agent.clone(),
                turn_id: "unrelated-retained-work".into(),
            }])
            .unwrap();
        let (reply, receive) = tokio::sync::oneshot::channel();
        actor
            .handle_request(
                super::super::AsyncRuntimeRequest::ClaimConfiguredAgentProviderTask {
                    agent_id: agent.clone(),
                    turn_id: turn.turn_id.clone(),
                    preparation,
                    reply,
                },
            )
            .await;
        let refusal = receive.await.unwrap();
        assert!(
            !matches!(&refusal, Ok(Some(_))),
            "admission must withhold the worker dispatch"
        );
        assert_eq!(
            actor.timers.next_provider_claim_generation, 1,
            "setup: request must reach actual provisional claim recording"
        );
        assert!(!actor.timers.provider_claim.contains_key(&turn.turn_id));
        assert!(!actor.service.agent_provider_task_is_owned(&turn.turn_id));
        assert!(actor.side_effects.iter().any(|effect| matches!(effect,
            RuntimeSideEffect::DispatchAgentProvider { turn_id, .. } if turn_id == "unrelated-retained-work")));
        let (refused_turn, refused_generation, refused_error) = actor.service
            .provider_lease_refusal_for_tests()
            .expect("setup: must observe actual timer refusal, not a source-stage/claim-precondition error");
        assert_eq!(refused_turn, &turn.turn_id);
        assert_eq!(
            *refused_generation,
            actor.timers.next_provider_claim_generation
        );
        assert_eq!(
            refused_error.kind(),
            MezError::invalid_state("type oracle").kind()
        );
        assert!(
            refused_error
                .message()
                .starts_with(SIDE_EFFECT_QUEUE_FULL_PREFIX)
        );

        // Desired baseline oracle: inspect immediately after the real request,
        // before shutdown/expiry/retry can alter custody or hide the boundary.
        let (retained_execution, retained_context, primary) = actor.service
            .original_completion_source_for_tests(&turn.turn_id, &group)
            .expect("F668_CUSTODY: exact original RuntimeOnly G1 result and context lost at undispatched lease refusal");
        assert_eq!(
            retained_execution, original_execution,
            "original canonical result/request must not become provider-error source"
        );
        assert!(
            retained_context
                .chronology()
                .starts_with(original_context.chronology()),
            "original canonical context events and metadata must remain owned in order"
        );
        let primary = primary
            .or_else(|| refusal.err())
            .expect("exact typed refusal must remain reported or retained");
        assert_eq!(primary.kind(), refused_error.kind());
        assert_eq!(primary.message(), refused_error.message());
        let terminal = actor
            .service
            .agent_turn_ledger()
            .turn(&turn.turn_id)
            .is_some_and(|turn| {
                matches!(
                    turn.state,
                    AgentTurnState::Failed | AgentTurnState::Interrupted
                )
            });
        assert!(
            terminal || actor.service.ensure_completion_admission_open().is_err(),
            "nonterminal refusal needs the existing bounded retained-failure owner, not an unleased runnable claim"
        );
        assert!(!actor.service.agent_provider_task_is_pending(&turn.turn_id));
        assert!(actor.service.pending_approved_external_actions().is_empty());
    }

    /// A full non-droppable side-effect queue rejects the mandatory auxiliary
    /// retry timer before dispatch and settles its exact pending owner. It must
    /// not leave a compacting marker or a waiting ordinary turn behind.
    #[tokio::test]
    async fn compaction_transport_retry_admission_settles_on_queue_pressure() {
        let mut actor = claimed_provider_actor();
        let profile = actor
            .service
            .agent_turn_model_profile("turn-1")
            .unwrap()
            .clone();
        let request = mez_agent::session_title::session_title_request(
            &profile,
            "agent-%1",
            &mez_agent::session_title::SessionTitleGenerationInputs {
                objective: Some("frozen source"),
                ..Default::default()
            },
            mez_agent::AllowedActionSet::say_only(),
        );
        actor
            .service
            .queue_agent_compaction_task(crate::runtime::RuntimeAgentCompactionTask {
                task_generation: 0,
                compaction_epoch: 0,
                transport_retry_attempts: 0,
                transport_retry_delay_ms: None,
                summary_correction: Default::default(),
                pane_id: "%1".into(),
                conversation_id: actor
                    .service
                    .agent_shell_store()
                    .get("%1")
                    .unwrap()
                    .session_id
                    .clone(),
                accounting_origin: crate::storage::token_usage::AccountingOrigin::Unattributed,
                source: "provider-context-limit".into(),
                transcript_entries: 0,
                compacted_through_sequence: None,
                frozen_compaction_rows: Vec::new(),
                retained_transcript_entries: 0,
                summarized_entries: 1,
                model_profile_name: "default".into(),
                model_profile: profile,
                request,
                preserve_summary_output_budget: true,
                manual_retry_source: None,
                manual_final_retry: None,
                candidate_context: None,
                resume_turn_id: Some("turn-1".into()),
                target: crate::runtime::RuntimeAgentCompactionTarget::Conversation,
                conversation_chunks: None,
                compaction_request_shape: None,
            });
        let task = actor
            .service
            .take_pending_agent_compaction_task("%1")
            .unwrap();
        let generation = task.task_generation;
        actor.service.claim_agent_compaction_task_state("%1", task);
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
        let timer = transition.side_effects.into_iter().find(|effect| matches!(effect,
            RuntimeSideEffect::ScheduleTimer { key, .. } if key.kind == RuntimeTimerKind::CompactionRetry
        )).unwrap();
        let retry = actor
            .service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        // A one-entry queue occupied by an unrelated non-droppable timer.
        actor.side_effect_buffer = 1;
        actor
            .queue_runtime_side_effects(vec![RuntimeSideEffect::ScheduleTimer {
                key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderRetry, "unrelated", 1),
                delay_ms: 10,
            }])
            .unwrap();
        assert!(
            !actor
                .admit_worker_claim_lease(
                    WorkerClaimLease::CompactionRetry {
                        pane_id: "%1",
                        generation: retry,
                    },
                    vec![timer]
                )
                .unwrap()
        );
        assert!(!actor.service.agent_is_compacting("%1"));
        assert!(
            actor
                .service
                .pending_agent_compaction_task_generation("%1")
                .is_none()
        );
        assert_eq!(
            actor
                .service
                .agent_turn_ledger()
                .turn("turn-1")
                .unwrap()
                .state,
            mez_agent::AgentTurnState::Failed
        );
        assert!(actor.timers.compaction_retry.is_empty());
    }

    /// A stale owner cannot enqueue a timer or settle a replacement claim. The
    /// replacement can still admit its exact positive lease afterward.
    #[tokio::test]
    async fn claim_admission_rejects_stale_owner_without_touching_replacement() {
        let mut actor = claimed_provider_actor();
        let agent = AgentId::opaque("agent-%1").unwrap();
        let old = WorkerClaimLease::Provider {
            agent_id: &agent,
            turn_id: "turn-1",
            generation: 1,
        };
        assert!(!actor.admit_worker_claim_lease(old, Vec::new()).unwrap());
        assert!(
            actor
                .service
                .agent_provider_claim_matches(&agent, "turn-1", 2)
        );
        let key = RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, "turn-1", 2);
        assert!(
            actor
                .admit_worker_claim_lease(
                    WorkerClaimLease::Provider {
                        agent_id: &agent,
                        turn_id: "turn-1",
                        generation: 2
                    },
                    vec![RuntimeSideEffect::ScheduleTimer {
                        key: key.clone(),
                        delay_ms: 30_000
                    }],
                )
                .unwrap()
        );
        assert_eq!(actor.timers.provider_claim.get("turn-1"), Some(&key));
    }

    /// Missing, duplicated, zero-delay, and wrong-generation lease effects
    /// fail before dispatch and retire only the exact undispatched claim.
    #[tokio::test]
    async fn claim_admission_rejects_invalid_provider_lease_effects() {
        let agent = AgentId::opaque("agent-%1").unwrap();
        let timer = |generation, delay_ms| RuntimeSideEffect::ScheduleTimer {
            key: RuntimeTimerKey::new(RuntimeTimerKind::ProviderClaim, "turn-1", generation),
            delay_ms,
        };
        for effects in [
            Vec::new(),
            vec![timer(2, 0)],
            vec![timer(1, 30_000)],
            vec![timer(2, 30_000), timer(2, 30_000)],
        ] {
            let mut actor = claimed_provider_actor();
            assert!(
                !actor
                    .admit_worker_claim_lease(
                        WorkerClaimLease::Provider {
                            agent_id: &agent,
                            turn_id: "turn-1",
                            generation: 2
                        },
                        effects,
                    )
                    .unwrap()
            );
            assert!(!actor.service.agent_provider_task_is_owned("turn-1"));
            assert_eq!(
                actor
                    .service
                    .agent_turn_ledger()
                    .turn("turn-1")
                    .unwrap()
                    .state,
                mez_agent::AgentTurnState::Failed
            );
            assert!(actor.timers.provider_claim.is_empty());
        }
    }

    /// Builds one exact undispatched claim without provider or credential I/O.
    fn claimed_provider_actor() -> AsyncRuntimeSessionActor {
        let mut service = RuntimeServiceFixture::new().build();
        let primary = service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .agent_shell_store_mut()
            .enter_or_resume("%1")
            .unwrap();
        service
            .execute_agent_shell_command(&primary, "lease admission")
            .unwrap();
        service
            .record_claimed_agent_provider_generation_for_tests("turn-1", 2)
            .unwrap();
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default())
            .unwrap()
            .1
    }
}
