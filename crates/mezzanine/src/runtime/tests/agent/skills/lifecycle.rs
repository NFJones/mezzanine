//! Explicit skill input through real command admission and provider execution.
//! Only inference is replaced; discovery, task ownership, persistence and model
//! request assembly remain production behavior with and without a configured
//! transcript store.

use super::*;

struct CaptureProvider(RefCell<Vec<mez_agent::ModelRequest>>);

impl ModelProvider for CaptureProvider {
    fn provider_id(&self) -> &str {
        "runtime-batch"
    }

    fn api_compatibility(&self) -> mez_agent::ProviderApiCompatibility {
        mez_agent::ProviderApiCompatibility::OpenAiResponses
    }

    fn send_request(&self, request: &mez_agent::ModelRequest) -> Result<mez_agent::ModelResponse> {
        self.0.borrow_mut().push(request.clone());
        Ok(mez_agent::ModelResponse {
            provider: request.provider.clone(),
            model: request.model.clone(),
            raw_text: "skill lifecycle task completed".into(),
            usage: Default::default(),
            latest_request_usage: None,
            quota_usage: Vec::new(),
            provider_transcript_events: Vec::new(),
            action_batch: Some(mez_agent::MaapBatch {
                rationale: "complete original admitted task".into(),
                actions: vec![mez_agent::AgentAction {
                    id: "finish".into(),
                    payload: mez_agent::AgentActionPayload::Say {
                        status: mez_agent::SayStatus::Final,
                        text: "skill lifecycle task completed".into(),
                        content_type: mez_agent::AGENT_OUTPUT_TEXT_PLAIN_CONTENT_TYPE.into(),
                    },
                }],
            }),
        })
    }
}

fn fixture(stored: bool) -> (PathBuf, RuntimeSessionService, mez_core::ids::ClientId) {
    let root = temp_root("skill-task-lifecycle");
    fs::create_dir_all(root.join("skills/review")).unwrap();
    fs::write(
        root.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: Lifecycle workflow\n---\nSKILL_LIFECYCLE_BODY\n",
    )
    .unwrap();
    let mut service = test_runtime_service();
    service.set_config_root(root.clone());
    if stored {
        service.set_agent_transcript_store(AgentTranscriptStore::new(root.join("transcripts")));
    }
    assert_eq!(service.agent_transcript_store().is_some(), stored);
    service.replace_config_layers(vec![ConfigLayer {
        name: "skill-task-lifecycle".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider=\"runtime-batch\"\ndefault_model_profile=\"skill-test\"\nshell_mode=\"pane\"\n[providers.runtime-batch]\nkind=\"openai\"\nmodels=[\"test\"]\ndefault_model=\"test\"\n[model_profiles.skill-test]\nprovider=\"runtime-batch\"\nmodel=\"test\"\ncontext_window_tokens=40000\n".into(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 30).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    // NoStore retains the canonical body in memory. A non-ephemeral session
    // without a transcript store has no cross-turn history source at all.
    service
        .agent_shell_store_mut()
        .ensure_session("%1")
        .unwrap()
        .ephemeral = !stored;
    (root, service, primary)
}

fn submit(service: &mut RuntimeSessionService, primary: &mez_core::ids::ClientId, prompt: &str) {
    service
        .execute_agent_shell_command(primary, prompt)
        .unwrap();
    service
        .run_pending_deferred_agent_command_for_tests()
        .unwrap();
}

/// Drain only actual accepted work. Correcting input may be consumed by the
/// current task or its queued successor; both must reach the production request
/// boundary and settle without replaying the inference fixture indefinitely.
fn finish_ready(service: &mut RuntimeSessionService, provider: &CaptureProvider) {
    for _ in 0..3 {
        let Some(turn) = service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .clone()
        else {
            break;
        };
        let profile = service.agent_turn_model_profile(&turn).unwrap().clone();
        assert_eq!(
            service
                .execute_agent_turn_with_provider(&turn, provider, profile)
                .unwrap()
                .terminal_state,
            AgentTurnState::Completed
        );
        for work in service.claim_bookkeeping_candidates() {
            let history = work.execute();
            assert!(
                service
                    .complete_bookkeeping_candidate(work, history)
                    .unwrap()
            );
        }
    }
    assert!(
        service
            .agent_shell_store()
            .get("%1")
            .unwrap()
            .running_turn_id
            .is_none()
    );
    assert!(service.pending_agent_provider_tasks().is_empty());
}

fn position(request: &mez_agent::ModelRequest, source: ContextSourceKind, text: &str) -> usize {
    request
        .messages
        .iter()
        .position(|message| message.source == source && message.content.contains(text))
        .unwrap_or_else(|| panic!("missing original fixture input {text}"))
}

#[test]
fn runtime_explicit_skill_lifecycle_preserves_consecutive_historical_inputs() {
    for stored in [false, true] {
        let (root, mut service, primary) = fixture(stored);
        let provider = CaptureProvider(RefCell::new(Vec::new()));
        submit(&mut service, &primary, "initial historical task");
        finish_ready(&mut service, &provider);
        submit(&mut service, &primary, "$review first skill task");
        finish_ready(&mut service, &provider);
        let skill_request = provider.0.borrow().last().unwrap().clone();
        let skill = position(
            &skill_request,
            ContextSourceKind::SkillInstruction,
            "SKILL_LIFECYCLE_BODY",
        );
        assert!(
            skill
                < position(
                    &skill_request,
                    ContextSourceKind::UserInstruction,
                    "$review first skill task"
                )
        );
        let original_skill = skill_request.messages[skill].clone();
        submit(&mut service, &primary, "later ordinary task");
        finish_ready(&mut service, &provider);
        let later = provider.0.borrow().last().unwrap().clone();
        let skill = position(
            &later,
            ContextSourceKind::SkillInstruction,
            "SKILL_LIFECYCLE_BODY",
        );
        assert_eq!(later.messages[skill], original_skill);
        assert!(
            position(
                &later,
                ContextSourceKind::UserInstruction,
                "initial historical task"
            ) < skill
        );
        assert!(
            skill
                < position(
                    &later,
                    ContextSourceKind::UserInstruction,
                    "later ordinary task"
                )
        );
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn runtime_explicit_skill_lifecycle_accepts_correcting_prompt_continuation() {
    for stored in [false, true] {
        let (root, mut service, primary) = fixture(stored);
        let provider = CaptureProvider(RefCell::new(Vec::new()));
        submit(&mut service, &primary, "historical completed task");
        finish_ready(&mut service, &provider);
        submit(&mut service, &primary, "unfinished original task");
        submit(&mut service, &primary, "steering before interruption");
        // Busy input is steering, not a new explicit skill task. Interrupt at
        // the public command boundary before submitting the correcting task.
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        submit(&mut service, &primary, "$review correcting task");
        finish_ready(&mut service, &provider);
        let requests = provider.0.borrow();
        let skill_request = requests
            .iter()
            .find(|request| {
                request.messages.iter().any(|message| {
                    message.source == ContextSourceKind::SkillInstruction
                        && message.content.contains("SKILL_LIFECYCLE_BODY")
                })
            })
            .expect("correcting skill input must reach the actual provider boundary");
        assert!(
            position(
                skill_request,
                ContextSourceKind::SkillInstruction,
                "SKILL_LIFECYCLE_BODY"
            ) < position(
                skill_request,
                ContextSourceKind::UserInstruction,
                "$review correcting task"
            )
        );
        assert!(requests.iter().any(|request| {
            request
                .messages
                .iter()
                .any(|message| message.content.contains("unfinished original task"))
        }));
        assert!(skill_request.messages.iter().any(|message| {
            message.source == ContextSourceKind::UserInstruction
                && message.content.contains("steering before interruption")
        }));
        drop(requests);
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }
}
