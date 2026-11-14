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
        RequestCase::Macro,
        RequestCase::Sandbox,
        RequestCase::SummaryRepair,
    ] {
        qualify(0, case).await;
    }
}

#[tokio::test]
async fn budget_production_dispatch_registers_actual_request_before_physical_send() {
    for case in [
        RequestCase::Execution,
        RequestCase::Macro,
        RequestCase::Sandbox,
        RequestCase::SummaryRepair,
    ] {
        qualify(100, case).await;
    }
}

#[derive(Clone, Copy, Debug)]
enum RequestCase {
    Execution,
    Macro,
    Sandbox,
    SummaryRepair,
}

impl RequestCase {
    fn model(self) -> &'static str {
        if matches!(self, Self::Macro | Self::Sandbox) {
            "auxiliary-request-model"
        } else {
            "actual-request-model"
        }
    }

    fn purposes(self) -> Vec<&'static str> {
        match self {
            Self::Execution => vec!["Execution"],
            Self::Macro => vec!["MacroJudge"],
            Self::Sandbox => vec!["SandboxAssessment"],
            Self::SummaryRepair => vec!["Execution", "FailureSummary", "FailureSummary"],
        }
    }
}

async fn qualify(input_limit: u64, case: RequestCase) {
    let Prepared {
        service,
        mut dispatch,
        store,
        member,
        mut root,
    } = prepared_case(BudgetLimits {
        input: TokenLimit::limited(input_limit).unwrap(),
        output: TokenLimit::Unlimited,
    })
    .await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    dispatch.model_profile.model = "actual-request-model".into();
    dispatch.provider = crate::runtime::RuntimeAgentProviderDispatchProvider::OpenAiCompatible(
        OpenAiCompatibleChatCompletionsProvider::without_auth(ReqwestProviderHttpTransport)
            .unwrap()
            .with_provider_id("local-chat")
            .unwrap()
            .with_streaming_options_for_test(false)
            .unwrap()
            .with_endpoint(endpoint),
    );
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
            let summary = matches!(case, RequestCase::SummaryRepair);
            let text = if summary && index == 1 {
                "not a summary batch".into()
            } else {
                serde_json::json!({"rationale":"fixture","actions":[{"type":"say","status":"final","content_type":"text/plain; charset=utf-8","text":"done"}]}).to_string()
            };
            let (status, body) = if summary && index == 0 {
                ("400 Bad Request", serde_json::json!({"error":{"message":"fixture schema refusal","type":"invalid_request_error"},"usage":{"prompt_tokens":11,"completion_tokens":3}}).to_string())
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
        let mut call = Box::pin(tokio::task::unconstrained(
            execute_runtime_agent_provider_dispatch(dispatch, None, sender, pending.clone()),
        ));
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
    drop((exit, handle, pending));
    root.cleanup();
    let served = served.expect("loopback server did not retire").unwrap();
    let (result, known, receipts, observed) = match result.unwrap() {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    };
    if input_limit == 0 {
        assert!(
            served.is_empty(),
            "exhausted root issued {} physical requests",
            served.len()
        );
        assert_eq!(result.unwrap_err().kind(), MezErrorKind::BudgetSuspended);
        assert_eq!((known, receipts), (UsageTotals::default(), 0));
    } else {
        assert!(result.is_ok(), "production dispatch failed: {result:?}");
        let purposes = case.purposes();
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
        if matches!(case, RequestCase::SummaryRepair) {
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
}
