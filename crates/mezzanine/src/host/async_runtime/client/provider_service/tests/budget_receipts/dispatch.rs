//! Actual configured dispatch -> loopback HTTP -> native receipt -> actor ACK.
//! No fixture installs a receipt source on the observer: production dispatch
//! must select it from its captured owner and the actual logical request.

use super::*;
use crate::integrations::http::ReqwestProviderHttpTransport;
use futures_util::FutureExt;
use mez_agent::token_budget::TokenLimit;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn budget_production_dispatch_fences_zero_before_physical_send() {
    for case in [
        RequestCase::Execution,
        RequestCase::Router,
        RequestCase::Compaction,
        RequestCase::Memory,
        RequestCase::Title,
        RequestCase::Macro,
        RequestCase::Sandbox,
        RequestCase::SummaryRepair,
    ] {
        Box::pin(qualify(0, case)).await;
    }
}

#[tokio::test]
async fn budget_production_dispatch_registers_actual_request_before_physical_send() {
    for case in [
        RequestCase::Execution,
        RequestCase::Router,
        RequestCase::Compaction,
        RequestCase::Memory,
        RequestCase::Title,
        RequestCase::Macro,
        RequestCase::Sandbox,
        RequestCase::SummaryRepair,
    ] {
        Box::pin(qualify(100, case)).await;
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestCase {
    Execution,
    Responses,
    DeepSeek,
    Anthropic,
    Router,
    Compaction,
    Memory,
    Title,
    Macro,
    Sandbox,
    SummaryRepair,
    ColdSummaryRepair,
}

impl RequestCase {
    fn model(self) -> &'static str {
        if matches!(self, Self::Router) {
            "router-request-model"
        } else if matches!(self, Self::Compaction) {
            "test-model"
        } else if matches!(self, Self::Macro | Self::Sandbox) {
            "auxiliary-request-model"
        } else {
            "actual-request-model"
        }
    }

    fn purposes(self) -> Vec<&'static str> {
        match self {
            Self::Execution | Self::Responses | Self::DeepSeek | Self::Anthropic => {
                vec!["Execution"]
            }
            Self::Router => vec!["Routing"],
            Self::Compaction => vec!["Compaction"],
            Self::Memory => vec!["Memory"],
            Self::Title => vec!["Title"],
            Self::Macro => vec!["MacroJudge"],
            Self::Sandbox => vec!["SandboxAssessment"],
            Self::SummaryRepair | Self::ColdSummaryRepair => {
                vec!["Execution", "FailureSummary", "FailureSummary"]
            }
        }
    }
}

#[tokio::test]
async fn budget_compaction_retired_claim_cannot_start_physical_http() {
    qualify_retirement(100, RequestCase::Compaction, CompactionRetirement::Stop).await;
}

#[tokio::test]
async fn budget_compaction_superseded_or_completed_claim_cannot_start_physical_http() {
    for retirement in [
        CompactionRetirement::Queued,
        CompactionRetirement::Claimed,
        CompactionRetirement::Completed,
    ] {
        Box::pin(qualify_retirement(100, RequestCase::Compaction, retirement)).await;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompactionRetirement {
    None,
    Stop,
    Queued,
    Claimed,
    Completed,
}

async fn qualify(input_limit: u64, case: RequestCase) {
    Box::pin(qualify_retirement(
        input_limit,
        case,
        CompactionRetirement::None,
    ))
    .await;
}

/// Exercise each actual inference adapter through production dispatch's own
/// captured request source, not an already-issued fixture receipt factory.
/// Zero must produce no HTTP; equality and allowance retain one exact 11/3
/// exchange and its original durable settlement.
#[tokio::test]
async fn budget_production_dispatch_all_unary_dialects_obey_native_admission() {
    for case in [
        RequestCase::Execution,
        RequestCase::Responses,
        RequestCase::DeepSeek,
        RequestCase::Anthropic,
    ] {
        for limit in [0, 11, 100] {
            Box::pin(qualify(limit, case)).await;
        }
    }
}

async fn qualify_retirement(input_limit: u64, case: RequestCase, retirement: CompactionRetirement) {
    let retire_compaction = retirement != CompactionRetirement::None;
    let cold = matches!(case, RequestCase::ColdSummaryRepair);
    let prepared = Box::pin(prepared_case(BudgetLimits {
        input: TokenLimit::limited(input_limit).unwrap(),
        output: TokenLimit::Unlimited,
    }))
    .await;
    let prepared = if cold {
        Box::pin(super::cold_summary::restore(prepared)).await
    } else {
        prepared
    };
    let Prepared {
        mut service,
        mut dispatch,
        store,
        member,
        mut root,
    } = prepared;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    dispatch.model_profile.model = "actual-request-model".into();
    use crate::integrations::agent::provider::{
        AnthropicMessagesProvider, DeepSeekChatCompletionsProvider, OpenAiResponsesProvider,
    };
    use crate::runtime::RuntimeAgentProviderDispatchProvider as Provider;
    dispatch.provider = match case {
        RequestCase::Responses => Provider::OpenAi(
            OpenAiResponsesProvider::with_endpoint_headers_and_stream(
                "fixture-key",
                &endpoint,
                10_000,
                Default::default(),
                false,
                ReqwestProviderHttpTransport,
            )
            .unwrap(),
        ),
        RequestCase::DeepSeek => Provider::DeepSeek(
            DeepSeekChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
                .unwrap()
                .with_endpoint(endpoint)
                .with_stream(false),
        ),
        RequestCase::Anthropic => Provider::Anthropic(
            AnthropicMessagesProvider::without_auth(ReqwestProviderHttpTransport)
                .unwrap()
                .with_endpoint(endpoint)
                .with_stream(false),
        ),
        _ => Provider::OpenAiCompatible(
            OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
                .unwrap()
                .with_provider_id("local-chat")
                .unwrap()
                .with_streaming_options_for_test(false)
                .unwrap()
                .with_endpoint(endpoint),
        ),
    };
    if matches!(
        case,
        RequestCase::Responses | RequestCase::DeepSeek | RequestCase::Anthropic
    ) {
        dispatch.model_profile.provider = dispatch.provider.provider_id().into();
    }
    if matches!(case, RequestCase::Router) {
        let target = |size: &str| mez_agent::AutoSizingTargetProfile {
            size: size.into(),
            profile_name: size.into(),
            profile: dispatch.model_profile.clone(),
            supported_reasoning_efforts: vec!["high".into()],
        };
        let mut router_profile = dispatch.model_profile.clone();
        router_profile.model = case.model().into();
        dispatch.auto_sizing = Some(mez_agent::AutoSizingDispatch {
            router_profile_name: "fixture-router".into(),
            router_profile,
            default_profile_name: "default".into(),
            default_profile: dispatch.model_profile.clone(),
            small: target("small"),
            medium: target("medium"),
            large: target("large"),
            turn_metadata: None,
            allowed_reasoning_efforts: vec!["high".into()],
            fallback_policy: mez_agent::AutoSizingFallbackPolicy::UseDefaultProfile,
        });
        dispatch.auto_sizing_provider = Some(dispatch.provider.clone());
    }
    if matches!(case, RequestCase::Macro | RequestCase::Sandbox) {
        let mut request = crate::integrations::agent::context::assemble_model_request(
            &dispatch.model_profile,
            dispatch.provider.api_compatibility(),
            &dispatch.turn,
            dispatch.context.durable(),
        )
        .unwrap();
        request.model = case.model().into();
        if matches!(case, RequestCase::Macro) {
            request.interaction_kind = mez_agent::ModelInteractionKind::MacroJudge;
            dispatch.macro_judge_request = Some(request);
        } else {
            request.interaction_kind = mez_agent::ModelInteractionKind::SandboxFailureAssessment;
            dispatch.sandbox_failure_assessment_request = Some(request);
        }
    }
    // Queue and claim the real turn-less operation; do not borrow the ordinary
    // dispatch's owner to manufacture auxiliary authority.
    let mut memory = None;
    let mut title = None;
    let mut compaction = None;
    if matches!(
        case,
        RequestCase::Compaction | RequestCase::Memory | RequestCase::Title
    ) {
        let primary = service
            .session()
            .clients()
            .iter()
            .find(|client| service.session().is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
        service.set_config_root(store.path().parent().unwrap().to_owned());
        let source = if matches!(case, RequestCase::Compaction) {
            use crate::config::{ConfigFormat, ConfigLayer, ConfigScope};
            service.replace_config_layers(vec![ConfigLayer {
                name: "budget-compaction-capacity".into(), path: None,
                format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
                text: "[agents]\nrouting=false\nshell_mode=\"native\"\ndefault_provider=\"local-chat\"\ndefault_model_profile=\"default\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.local-chat]\nkind=\"openai-compatible\"\nbase_url=\"http://127.0.0.1:1/v1\"\nmodels=[\"test-model\"]\ndefault_model=\"test-model\"\n[model_profiles.default]\nprovider=\"local-chat\"\nmodel=\"test-model\"\ncontext_window_tokens=40000\n".into(),
            }]).unwrap();
            // Physical transcript rows alone do not make a live canonical
            // task eligible. Complete genuine work and admit its successor.
            let turn = crate::test_support::compaction_source::complete_task_and_admit_successor(
                &mut service,
                &primary,
                &format!("earlier public source {}", "evidence ".repeat(6_000)),
                "original completed evidence",
                "continue after compaction",
                mez_agent::ProviderApiCompatibility::OpenAiChatCompletions,
            );
            let error = MezError::invalid_state("provider context length exceeded")
                .with_provider_failure_json(
                    r#"{"status_code":400,"error":{"code":"context_length_exceeded"}}"#,
                );
            service
                .schedule_agent_provider_retry_transition(
                    &AgentId::opaque("agent-%1").unwrap(),
                    &turn,
                    ProviderErrorRetryClass::ContextLimit,
                    &error,
                )
                .unwrap()
                .unwrap();
            crate::runtime::RuntimeBudgetAuxiliarySource::Compaction {
                pane: "%1".into(),
                generation: service
                    .pending_agent_compaction_task_generation("%1")
                    .unwrap(),
            }
        } else if matches!(case, RequestCase::Memory) {
            service
                .execute_agent_shell_command(&primary, "/remember Preserve original accounting.")
                .unwrap();
            crate::runtime::RuntimeBudgetAuxiliarySource::Remember {
                pane: "%1".into(),
                observation: service
                    .pending_agent_remember_observation_id("%1")
                    .unwrap()
                    .into(),
            }
        } else {
            // The original prompt queued a title before test-only membership
            // activation. Retire that unissued task; a fresh queue captures
            // the real auxiliary membership instead of borrowing its owner.
            service.cancel_agent_session_title_task(&member.conversation);
            service
                .schedule_runtime_agent_session_title(
                    &member.conversation,
                    Some("Qualify original title accounting"),
                )
                .unwrap();
            crate::runtime::RuntimeBudgetAuxiliarySource::Title {
                conversation: member.conversation.clone(),
                dispatch: service
                    .pending_agent_session_title_dispatch_id(&member.conversation)
                    .unwrap()
                    .into(),
            }
        };
        let work = service
            .prepare_runtime_budget_auxiliary(&source)
            .unwrap()
            .unwrap();
        let prepared = RuntimeSessionService::execute_agent_provider_preparation(work).await;
        service.apply_agent_provider_preparation(prepared).unwrap();
        match source {
            crate::runtime::RuntimeBudgetAuxiliarySource::Remember { pane, observation } => {
                let mut claimed = service
                    .claim_agent_remember_task(&pane, &observation)
                    .unwrap()
                    .unwrap();
                claimed.provider = dispatch.provider.clone();
                claimed.task.request.model = case.model().into();
                assert_eq!(claimed.budget_owner.as_ref().unwrap().member, member);
                memory = Some(claimed);
            }
            crate::runtime::RuntimeBudgetAuxiliarySource::Title {
                conversation,
                dispatch: id,
            } => {
                let mut claimed = service
                    .claim_agent_session_title_dispatch(&conversation, &id)
                    .unwrap()
                    .unwrap();
                claimed.provider = dispatch.provider.clone();
                claimed.task.request.model = case.model().into();
                assert_eq!(claimed.budget_owner.as_ref().unwrap().member, member);
                title = Some(claimed);
            }
            crate::runtime::RuntimeBudgetAuxiliarySource::Compaction { pane, generation } => {
                let mut claimed = service
                    .claim_agent_compaction_task(&pane, generation)
                    .unwrap()
                    .unwrap();
                claimed.provider = dispatch.provider.clone();
                assert_eq!(claimed.budget_owner.as_ref().unwrap().member, member);
                compaction = Some(claimed);
            }
        }
    }
    if retirement == CompactionRetirement::Stop {
        let primary = service
            .session()
            .clients()
            .iter()
            .find(|client| service.session().is_attached_primary(&client.id))
            .unwrap()
            .id
            .clone();
        service
            .execute_agent_shell_command(&primary, "/stop")
            .unwrap();
    }
    if matches!(
        retirement,
        CompactionRetirement::Queued | CompactionRetirement::Claimed
    ) {
        let original = &compaction.as_ref().unwrap().task;
        let generation = original.task_generation;
        service.queue_agent_compaction_task(original.clone());
        let successor = service
            .pending_agent_compaction_task_generation("%1")
            .unwrap();
        assert_ne!(successor, generation);
        if retirement == CompactionRetirement::Claimed {
            let claimed = service
                .claim_agent_compaction_task("%1", successor)
                .unwrap()
                .unwrap();
            assert_eq!(claimed.task.task_generation, successor);
            drop(claimed);
        }
    }
    if retirement == CompactionRetirement::Completed {
        let generation = compaction.as_ref().unwrap().task.task_generation;
        assert!(
            service
                .finish_agent_compaction_task("%1", generation)
                .is_some()
        );
    }
    // Actual auxiliary claims must not become generic same-conversation
    // request factories. Capture refusals now, but assert only after native
    // actors, HTTP work and the exact fixture root have retired, including RED.
    let auxiliary_owner = memory
        .as_ref()
        .and_then(|dispatch| dispatch.budget_owner.as_ref())
        .or_else(|| {
            title
                .as_ref()
                .and_then(|dispatch| dispatch.budget_owner.as_ref())
        })
        .or_else(|| {
            compaction
                .as_ref()
                .and_then(|dispatch| dispatch.budget_owner.as_ref())
        });
    let purpose_refusals = auxiliary_owner.map(|owner| {
        [
            BudgetAttemptPurpose::Execution,
            BudgetAttemptPurpose::Routing,
            BudgetAttemptPurpose::Compaction,
            BudgetAttemptPurpose::Memory,
            BudgetAttemptPurpose::Title,
            BudgetAttemptPurpose::MacroJudge,
            BudgetAttemptPurpose::SandboxAssessment,
            BudgetAttemptPurpose::FailureSummary,
        ]
        .into_iter()
        .map(|purpose| {
            let expected = matches!(
                (case, purpose),
                (RequestCase::Compaction, BudgetAttemptPurpose::Compaction)
                    | (RequestCase::Memory, BudgetAttemptPurpose::Memory)
                    | (RequestCase::Title, BudgetAttemptPurpose::Title)
            );
            let accepted = owner
                .prepare_attempt(
                    mez_agent::ModelTokenUsageKey::new("local-chat", case.model()),
                    purpose,
                )
                .is_ok();
            (format!("{purpose:?}"), accepted, expected)
        })
        .collect::<Vec<_>>()
    });
    let held_isolation = if let Some(owner) = auxiliary_owner.filter(|_| input_limit == 0) {
        let purpose = match case {
            RequestCase::Compaction => BudgetAttemptPurpose::Compaction,
            RequestCase::Memory => BudgetAttemptPurpose::Memory,
            RequestCase::Title => BudgetAttemptPurpose::Title,
            _ => unreachable!("only actual auxiliary dispatches have auxiliary owners"),
        };
        let work = owner
            .prepare_attempt(
                mez_agent::ModelTokenUsageKey::new("local-chat", case.model()),
                purpose,
            )
            .unwrap();
        let admission = work.admission_owner();
        let outcome = admission.claim().await.unwrap();
        let capture = admission.capture_held().await.unwrap();
        Some(
            matches!(outcome, RuntimeBudgetAttemptAdmission::Held(_))
                && capture.matches_owner(owner)
                && capture.matches_owner(&owner.clone())
                && !capture.matches_owner(dispatch.budget_owner.as_ref().unwrap()),
        )
    } else {
        None
    };
    let allocation_was_inert = rows(&store, "budget_attempts") == 0
        && rows(&store, "budget_admissions") == 0
        && rows(&store, "budget_receipts") == 0;
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let writer = store.clone();
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        loop {
            let (mut socket, _) = tokio::select! {
                biased;
                _ = &mut stopped => break,
                accepted = listener.accept() => accepted.unwrap(),
            };
            assert!(requests.len() < 4, "unexpected physical retry");
            let mut bytes = Vec::new();
            let end = loop {
                if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
                    break index + 4;
                }
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0 && bytes.len() + read <= 1024 * 1024);
                bytes.extend_from_slice(&chunk[..read]);
            };
            let headers = std::str::from_utf8(&bytes[..end]).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(end + length <= 1024 * 1024);
            while bytes.len() < end + length {
                let mut chunk = [0; 4096];
                let read = socket.read(&mut chunk).await.unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&chunk[..read]);
            }
            let request: serde_json::Value =
                serde_json::from_slice(&bytes[end..end + length]).unwrap();
            let index = requests.len();
            let body: String = rusqlite::Connection::open(writer.path())
                .unwrap()
                .query_row(
                    "SELECT body FROM budget_attempts ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let attempt: serde_json::Value = serde_json::from_str(&body).unwrap();
            requests.push((
                request["model"].as_str().unwrap().to_string(),
                rows(&writer, "budget_attempts"),
                attempt["purpose"].as_str().unwrap().to_string(),
                attempt["model"]["model"].as_str().unwrap().to_string(),
            ));
            let summary = matches!(
                case,
                RequestCase::SummaryRepair | RequestCase::ColdSummaryRepair
            );
            let text = if matches!(case, RequestCase::Router) {
                serde_json::json!({"version":1,"size":"small","reasoning_effort":"high","confidence":0.9,"rationale":"fixture"}).to_string()
            } else if matches!(case, RequestCase::Title) {
                "Original accounting title".into()
            } else if summary && index == 1 {
                "not a summary batch".into()
            } else {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string()
            };
            let (status, body) = if summary && index == 0 {
                ("400 Bad Request", serde_json::json!({"error":{"message":"fixture schema refusal","type":"invalid_request_error"},"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string())
            } else if matches!(case, RequestCase::Responses) {
                ("200 OK", serde_json::json!({"model":"provider-reported-model","status":"completed","output_text":text,"usage":{"input_tokens":11,"output_tokens":3}}).to_string())
            } else if matches!(case, RequestCase::Anthropic) {
                ("200 OK", serde_json::json!({"id":"fixture-message","type":"message","role":"assistant","model":"provider-reported-model","stop_reason":"tool_use","content":[{"type":"tool_use","id":"fixture-tool","name":mez_agent::MAAP_ACTION_BATCH_TOOL_NAME,"input":serde_json::from_str::<serde_json::Value>(&text).unwrap()}],"usage":{"input_tokens":11,"output_tokens":3}}).to_string())
            } else {
                ("200 OK", serde_json::json!({"model":"provider-reported-model","choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string())
            };
            socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
        requests
    });
    let pending = PendingProviderWireObservations::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut observed = Vec::new();
        let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender.clone())
            .with_pending_observations(pending.clone());
        // Separate boxed branches keep the auxiliary and ordinary runner
        // state machines out of the enclosing fixture's stack-resident future.
        let work: Pin<Box<dyn Future<Output = Result<()>> + '_>> = if let Some(dispatch) = memory {
            Box::pin(async move {
                execute_runtime_agent_remember_dispatch(dispatch, observer)
                    .await
                    .map(|_| ())
            })
        } else if let Some(dispatch) = title {
            Box::pin(async move {
                match execute_runtime_agent_session_title_dispatch(dispatch, observer)
                    .await
                    .0
                {
                    AgentSessionTitleOutcome::Generated(_) => Ok(()),
                    AgentSessionTitleOutcome::Rejected(reason) => {
                        Err(MezError::invalid_state(reason))
                    }
                }
            })
        } else if let Some(dispatch) = compaction {
            Box::pin(async move {
                let observer = observer.with_claim_generation(dispatch.task.task_generation);
                let owner = ProviderWorkerOwner::compaction(&dispatch);
                let mut workers = JoinSet::new();
                let mut owners = Default::default();
                let admitted = spawn_owned_agent_provider_worker(
                    &mut workers,
                    &mut owners,
                    Some(owner),
                    async move {
                        execute_runtime_agent_compaction_dispatch(dispatch, observer)
                            .await
                            .map(|_| None)
                    },
                );
                assert!(admitted.is_some(), "actual compactor producer registration");
                workers.join_next().await.unwrap().unwrap().map(|_| ())
            })
        } else {
            Box::pin(async {
                execute_runtime_agent_provider_dispatch(dispatch, None, sender, pending.clone())
                    .await
                    .map(|outcome| {
                        if cold {
                            let RuntimeAgentProviderWorkerOutcome::Execution(execution) = outcome
                            else {
                                panic!("cold failure summary unexpectedly routed");
                            };
                            assert_eq!(execution.terminal_state, mez_agent::AgentTurnState::Failed);
                            let attempts = input_limit.div_ceil(11).min(3);
                            assert_eq!(execution.response.usage.input_tokens, 11 * attempts);
                            assert_eq!(execution.response.usage.output_tokens, 3 * attempts);
                        }
                    })
            })
        };
        let mut call = Box::pin(tokio::task::unconstrained(work));
        let result = loop {
            tokio::select! {
                biased;
                result = &mut call => break result,
                Some(observation) = receiver.recv() => {
                    observed.push((observation.purpose, observation.interaction_kind.clone()));
                    submit_pending_provider_observation(&handle, &pending, observation).await.unwrap();
                }
            }
        };
        flush_pending_provider_observations(&handle, &pending)
            .await
            .unwrap();
        (
            result,
            store.budget_account(&member.root).unwrap().known,
            rows(&store, "budget_receipts"),
            observed,
        )
    };
    let control = async {
        let result = tokio::time::timeout(
            Duration::from_secs(15),
            std::panic::AssertUnwindSafe(client).catch_unwind(),
        )
        .await;
        let _ = stop.send(());
        let mut server = server;
        let served = match tokio::time::timeout(Duration::from_secs(10), &mut server).await {
            Ok(result) => Some(result),
            Err(_) => {
                server.abort();
                let _ = server.await;
                None
            }
        };
        handle.shutdown().await.unwrap();
        (result, served)
    };
    let ((result, served), mut exit) = tokio::join!(control, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    let outstanding: i64 = rusqlite::Connection::open(store.path()).unwrap().query_row(
        "SELECT COUNT(*) FROM (SELECT id FROM budget_attempts WHERE terminal=0 UNION SELECT attempt FROM budget_admissions WHERE retired_receipt IS NULL)",
        [], |row| row.get(0),
    ).unwrap();
    drop((exit, handle, pending));
    if cold
        || matches!(
            case,
            RequestCase::Responses | RequestCase::DeepSeek | RequestCase::Anthropic
        )
    {
        assert_eq!(outstanding, 0, "dispatch retained original exposure");
    }
    root.cleanup();
    let served = served.expect("loopback server did not retire").unwrap();
    let (result, known, receipts, observed) = match result.unwrap() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if input_limit == 0 || retire_compaction {
        assert!(
            served.is_empty(),
            "inhibited original request issued {} physical requests",
            served.len()
        );
        let error = result.unwrap_err();
        if matches!(case, RequestCase::Title) {
            // The existing title boundary sanitizes provider errors to this
            // bounded rejection. Native zero-send/zero-expense remains proved.
            assert_eq!(error.message(), "provider_error");
        } else {
            assert_eq!(
                error.kind(),
                MezErrorKind::BudgetSuspended,
                "{case:?}: {error:?}"
            );
        }
        assert_eq!(known, UsageTotals::default());
        if retire_compaction {
            assert!(receipts > 0, "original unused native grant must settle");
            assert_eq!(
                outstanding, 0,
                "original unused grant cannot retain exposure"
            );
        } else {
            assert_eq!(receipts, 0);
        }
    } else {
        let mut purposes = case.purposes();
        if cold {
            purposes.truncate(usize::try_from(input_limit.div_ceil(11)).unwrap());
        }
        if cold && purposes.len() == 1 {
            // No summary expense means the original HTTP failure remains the
            // causal result; a budget hold must not erase or relabel it.
            let error = result.as_ref().unwrap_err();
            assert_eq!(error.kind(), MezErrorKind::InvalidState);
            let failure: serde_json::Value =
                serde_json::from_str(error.provider_failure_json().unwrap()).unwrap();
            assert_eq!(failure["status_code"], 400);
            assert_eq!(error.provider_incurred_usage().input_tokens, 11);
            assert_eq!(error.provider_incurred_usage().output_tokens, 3);
        } else {
            assert!(result.is_ok(), "production dispatch failed: {result:?}");
        }
        let expected: Vec<_> = purposes
            .iter()
            .enumerate()
            .map(|(index, purpose)| {
                (
                    case.model().into(),
                    (index + 1) as i64,
                    (*purpose).into(),
                    case.model().into(),
                )
            })
            .collect();
        assert_eq!(served, expected, "{case:?}");
        if matches!(
            case,
            RequestCase::SummaryRepair | RequestCase::ColdSummaryRepair
        ) && purposes.len() == 3
        {
            assert_eq!(
                observed[1],
                (
                    ProviderRequestPurpose::FailureSummary,
                    "failure_summary".into()
                )
            );
            assert_eq!(
                observed[2],
                (ProviderRequestPurpose::FailureSummary, "maap_repair".into())
            );
        }
        assert_eq!(
            (known, receipts),
            (
                UsageTotals {
                    input: 11 * purposes.len() as u64,
                    output: 3 * purposes.len() as u64
                },
                2 * purposes.len() as i64
            )
        );
    }
    if let Some(purposes) = purpose_refusals {
        for (purpose, accepted, expected) in purposes {
            assert_eq!(accepted, expected, "{case:?} claimed owner: {purpose}");
        }
    }
    assert!(
        allocation_was_inert,
        "allocation/hold wrote expense or admission"
    );
    if let Some(isolated) = held_isolation {
        assert!(isolated, "{case:?} held owner borrowed ordinary authority");
    }
}

pub(super) async fn qualify_cold_summary(input: u64) {
    Box::pin(qualify(input, RequestCase::ColdSummaryRepair)).await;
}
