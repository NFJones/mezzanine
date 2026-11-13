//! Concrete chat decoding -> original native receipt -> real actor ACK.
//! Admission is explicitly prepared before the fake physical exchange; this
//! staged bridge does not claim production activation or retirement release.

mod admission;
mod internal_retry;
mod pre_send;
mod retirement;
mod transport;

use super::super::budget_receipts::BudgetObservationReceipts;
use super::*;
use crate::integrations::agent::provider::{
    OpenAiCompatibleChatCompletionsProvider, ProviderObservationReceiptFactory,
};
use crate::integrations::http::AsyncProviderHttpTransport;
use crate::runtime::{
    RuntimeBudgetAttemptAdmission, RuntimeBudgetIssuedAttempt, RuntimeSessionService,
};
use crate::storage::token_usage::{
    TokenUsageStore,
    budget::{
        membership::BudgetMembership,
        policy_projection::{BudgetConfigPolicy, BudgetPolicyPublication},
        receipts::BudgetAttemptPurpose,
    },
};
use mez_agent::token_budget::{BudgetLimits, BudgetReceiptId, UsageTotals};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Transport {
    usage: serde_json::Value,
    calls: Arc<AtomicUsize>,
}
impl AsyncProviderHttpTransport for Transport {
    fn send_async<'a>(
        &'a self,
        _: &'a mez_agent::ProviderHttpRequest,
    ) -> Pin<
        Box<
            dyn Future<Output = mez_agent::ProviderHttpResult<mez_agent::ProviderHttpResponse>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(mez_agent::ProviderHttpResponse {
                status_code: 200,
                headers: Default::default(),
                body: serde_json::json!({"model":"test-model", "choices":[{
                    "index":0,"finish_reason":"stop","message":{"role":"assistant",
                    "content":"{\"rationale\":\"fixture\",\"actions\":[{\"type\":\"say\",\"status\":\"final\",\"content_type\":\"text/plain; charset=utf-8\",\"text\":\"done\"}]}"}
                }],"usage":self.usage}).to_string(),
            })
        })
    }
}

struct Case {
    service: RuntimeSessionService,
    dispatch: RuntimeAgentProviderDispatch,
    issued: RuntimeBudgetIssuedAttempt,
    factory: Arc<BudgetObservationReceipts>,
    store: TokenUsageStore,
    member: BudgetMembership,
    root: super::budget_custody::FixtureRoot,
}

struct Prepared {
    service: RuntimeSessionService,
    dispatch: RuntimeAgentProviderDispatch,
    store: TokenUsageStore,
    member: BudgetMembership,
    root: super::budget_custody::FixtureRoot,
}

async fn prepared_case(limits: BudgetLimits) -> Prepared {
    let (mut service, agent, turn) = super::incomplete::prepared_fixture("durable wire ACK").await;
    let path = service
        .agent_transcript_store()
        .unwrap()
        .root()
        .parent()
        .unwrap()
        .to_owned();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = super::budget_custody::FixtureRoot(Some(path.clone()));
    let store = TokenUsageStore::new(path.join("usage.sqlite"));
    service.enable_runtime_budget_membership_for_tests();
    let preparation = service.prepare_agent_provider_work(&turn).unwrap();
    let prepared = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(prepared).unwrap();
    let mut dispatch = service
        .claim_configured_agent_provider_task(&agent, &turn)
        .unwrap()
        .unwrap();
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    dispatch.claim_generation = 7;
    let owner = dispatch.budget_owner.as_ref().unwrap();
    let member = owner.member.clone();
    let operation = BudgetPolicyPublication {
        namespace: member.namespace.clone(),
        id: BudgetReceiptId::new("wire-policy").unwrap(),
        expected_revision: 0,
        previous_digest: "a".repeat(64),
        proposed: BudgetConfigPolicy {
            source: "primary".into(),
            digest: "b".repeat(64),
            limits,
        },
        supersedes: None,
    };
    let writer = store.clone();
    tokio::task::spawn_blocking(move || {
        writer.begin_budget_policy_publication(&operation).unwrap();
        writer
            .reconcile_budget_policy_publication(&operation, &operation.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
    Prepared {
        service,
        dispatch,
        store,
        member,
        root,
    }
}

async fn case(purpose: BudgetAttemptPurpose) -> Case {
    let Prepared {
        service,
        dispatch,
        store,
        member,
        root,
    } = prepared_case(BudgetLimits::default()).await;
    let owner = dispatch.budget_owner.as_ref().unwrap();
    let attempt = owner
        .prepare_attempt(
            mez_agent::ModelTokenUsageKey::new("local-chat", "test-model"),
            purpose,
        )
        .unwrap();
    let issued = tokio::task::spawn_blocking(move || {
        let RuntimeBudgetAttemptAdmission::Authorized(permit) = attempt.authorize().unwrap() else {
            panic!("fresh native wire grant expected");
        };
        permit.consume().unwrap()
    })
    .await
    .unwrap();
    let factory = Arc::new(BudgetObservationReceipts::new(issued.registered().clone()));
    Case {
        service,
        dispatch,
        issued,
        factory,
        store,
        member,
        root,
    }
}

fn rows(store: &TokenUsageStore, table: &str) -> i64 {
    assert!(matches!(table, "budget_receipts" | "budget_admissions"));
    rusqlite::Connection::open(store.path())
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

async fn qualify(
    purpose: BudgetAttemptPurpose,
    wire_purpose: ProviderRequestPurpose,
    usage: serde_json::Value,
    expected_snapshot: mez_agent::token_budget::UsageSnapshot,
    refuse: bool,
) {
    let expected = UsageTotals {
        input: expected_snapshot.input.unwrap_or(0),
        output: expected_snapshot.output.unwrap_or(0),
    };
    let Case {
        service,
        dispatch,
        issued,
        factory,
        store,
        member,
        mut root,
    } = case(purpose).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
        usage,
        calls: calls.clone(),
    })
    .unwrap()
    .with_provider_id("local-chat")
    .unwrap();
    let request = crate::integrations::agent::context::assemble_model_request(
        &dispatch.model_profile,
        provider.api_compatibility(),
        &dispatch.turn,
        dispatch.context.durable(),
    )
    .unwrap();
    let pending = PendingProviderWireObservations::default();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
        .with_claim_generation(7)
        .with_pending_observations(pending.clone())
        .with_budget_receipts(factory.clone())
        .unwrap();
    let observed = ObservedAsyncModelProvider::new(&provider, &observer, wire_purpose);
    let (handle, actor) =
        crate::host::async_runtime::AsyncRuntimeSessionActor::new(service, Default::default())
            .unwrap();
    let client = async {
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        let observation = tokio::select! {
            biased;
            _ = &mut call => panic!("provider advanced without durable receipt and actor ACK"),
            observation = receiver.recv() => observation.unwrap(),
        };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(pending.snapshot(), vec![observation.clone()]);
        pending.acknowledge(&observation.request_id);
        assert_eq!(
            pending.len(),
            1,
            "actor ACK alone must not discard native receipt"
        );
        assert_eq!(rows(&store, "budget_receipts"), 0);
        assert_eq!(
            store.budget_account(&member.root).unwrap().known,
            UsageTotals::default()
        );
        let mut wrong = observation.clone();
        wrong.model = "foreign".into();
        assert!(
            submit_pending_provider_observation(&handle, &pending, wrong)
                .await
                .is_err()
        );
        if refuse {
            let connection = rusqlite::Connection::open(store.path()).unwrap();
            connection.execute_batch("CREATE TRIGGER refuse_wire_receipt BEFORE INSERT ON budget_receipts BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
            assert!(
                submit_pending_provider_observation(&handle, &pending, observation.clone())
                    .await
                    .is_err()
            );
            assert_eq!(rows(&store, "budget_receipts"), 0);
            assert!(futures_util::poll!(&mut call).is_pending());
            connection
                .execute_batch("DROP TRIGGER refuse_wire_receipt;")
                .unwrap();
            // A healed store does not silently repeat an uncertain write.
            assert!(
                submit_pending_provider_observation(&handle, &pending, observation.clone())
                    .await
                    .is_err()
            );
            factory.retry_after_absence().await.unwrap();
            assert_eq!(store.budget_account(&member.root).unwrap().known, expected);
        } else {
            // The journal and a fresh descriptor clone must resume the SAME
            // actual writer after both its first waiter and a checkpoint wait
            // disappear. The gate controls scheduling, not the SQL transaction.
            let work = issued
                .registered()
                .freeze_wire_receipt(&observation)
                .unwrap();
            let owner = work.commit_owner();
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, gate) = std::sync::mpsc::channel();
            let waiter = tokio::spawn(async move {
                owner
                    .commit_with_for_test(move || {
                        let _ = started.send(());
                        gate.recv()
                            .map_err(|_| MezError::invalid_state("fixture writer gate closed"))?;
                        work.execute()
                    })
                    .await
            });
            ready.await.unwrap();
            waiter.abort();
            assert!(waiter.await.unwrap_err().is_cancelled());
            {
                let checkpoint = pending.checkpoint(&observation);
                tokio::pin!(checkpoint);
                assert!(futures_util::poll!(&mut checkpoint).is_pending());
            }
            assert_eq!(rows(&store, "budget_receipts"), 0);
            assert!(futures_util::poll!(&mut call).is_pending());
            release.send(()).unwrap();
            pending.checkpoint(&observation).await.unwrap();
            assert_eq!(pending.len(), 1, "durable receipt alone is not actor ACK");
            assert_eq!(store.budget_account(&member.root).unwrap().known, expected);
        }
        submit_pending_provider_observation(&handle, &pending, observation.clone())
            .await
            .unwrap();
        assert!(pending.is_empty());
        assert!(call.await.unwrap().action_batch.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(rows(&store, "budget_receipts"), 1);
        assert_eq!(
            rows(&store, "budget_admissions"),
            1,
            "receipt ACK never retires exposure"
        );
        assert_eq!(store.budget_account(&member.root).unwrap().known, expected);
        // Same original source, even through another factory, keeps exact ACK.
        let duplicate = BudgetObservationReceipts::new(issued.registered().clone());
        duplicate
            .freeze(&observation)
            .unwrap()
            .checkpoint()
            .await
            .unwrap();
        assert_eq!(rows(&store, "budget_receipts"), 1);
        let mut second = observation.clone();
        second.request_id.push_str("-second");
        second.attempt_index += 1;
        assert!(duplicate.freeze(&second).is_err());
        let another_factory = BudgetObservationReceipts::new(issued.registered().clone());
        assert!(another_factory.freeze(&second).is_err());
        let connection = rusqlite::Connection::open(store.path()).unwrap();
        let body: String = connection
            .query_row("SELECT body FROM budget_receipts", [], |row| row.get(0))
            .unwrap();
        let receipt: crate::storage::token_usage::budget::receipts::BudgetReceipt =
            serde_json::from_str(&body).unwrap();
        assert_eq!(receipt.snapshot, expected_snapshot);
        let terminal: bool = connection
            .query_row("SELECT terminal FROM budget_attempts", [], |row| row.get(0))
            .unwrap();
        assert!(
            !terminal,
            "wire subtotals cannot fabricate final coverage or retirement"
        );
        handle.shutdown().await.unwrap();
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    exit.service.terminate_all_pane_processes().unwrap();
    drop((exit, observer, factory, issued, dispatch, provider, handle));
    root.cleanup();
}

#[tokio::test]
async fn budget_wire_receipts_all_purposes_commit_before_real_actor_ack() {
    for (purpose, wire) in [
        (
            BudgetAttemptPurpose::Execution,
            ProviderRequestPurpose::Execution,
        ),
        (
            BudgetAttemptPurpose::Routing,
            ProviderRequestPurpose::Routing,
        ),
        (
            BudgetAttemptPurpose::Compaction,
            ProviderRequestPurpose::Compaction,
        ),
        (BudgetAttemptPurpose::Memory, ProviderRequestPurpose::Memory),
        (
            BudgetAttemptPurpose::Title,
            ProviderRequestPurpose::Auxiliary,
        ),
        (
            BudgetAttemptPurpose::MacroJudge,
            ProviderRequestPurpose::Auxiliary,
        ),
        (
            BudgetAttemptPurpose::SandboxAssessment,
            ProviderRequestPurpose::Auxiliary,
        ),
        (
            BudgetAttemptPurpose::FailureSummary,
            ProviderRequestPurpose::Auxiliary,
        ),
    ] {
        qualify(
            purpose,
            wire,
            serde_json::json!({"prompt_tokens":11,"completion_tokens":3}),
            prefix(Some(11), Some(3)),
            false,
        )
        .await;
    }
}

#[tokio::test]
async fn budget_wire_receipt_storage_refusal_then_explicit_original_recovery() {
    qualify(
        BudgetAttemptPurpose::Execution,
        ProviderRequestPurpose::Execution,
        serde_json::json!({"prompt_tokens":11,"completion_tokens":3}),
        prefix(Some(11), Some(3)),
        true,
    )
    .await;
}

#[tokio::test]
async fn budget_wire_receipt_partial_zero_and_subsets_never_invent_complete_totals() {
    for (usage, expected) in [
        (
            serde_json::json!({"prompt_tokens":0,"completion_tokens":0}),
            prefix(Some(0), Some(0)),
        ),
        (
            serde_json::json!({"prompt_tokens":11}),
            prefix(Some(11), None),
        ),
        (
            serde_json::json!({"completion_tokens":3}),
            prefix(None, Some(3)),
        ),
        (serde_json::json!({}), prefix(None, None)),
        (
            serde_json::json!({"prompt_tokens_details":{"cached_tokens":4}}),
            prefix(Some(4), None),
        ),
        (
            serde_json::json!({"completion_tokens_details":{"reasoning_tokens":2}}),
            prefix(None, Some(2)),
        ),
    ] {
        qualify(
            BudgetAttemptPurpose::Execution,
            ProviderRequestPurpose::Execution,
            usage,
            expected,
            false,
        )
        .await;
    }
}

fn prefix(input: Option<u64>, output: Option<u64>) -> mez_agent::token_budget::UsageSnapshot {
    mez_agent::token_budget::UsageSnapshot {
        input,
        output,
        terminal: false,
    }
}

/// Mutate only content-free evidence decoded by a real adapter. Invalid capture
/// must remain refused through descriptor clones, without writing a receipt or
/// borrowing a different counter's presence. Cleanup precedes the refusal oracle
/// so a RED result cannot strand this fixture's original native process.
#[tokio::test]
async fn budget_wire_receipt_invalid_counter_evidence_is_sticky_and_inert() {
    type Change = fn(&mut ProviderWireRequestObservation);
    let changes: [(&str, Change); 9] = [
        ("cache cannot qualify unrelated primary input", |o| {
            o.usage_presence.as_mut().unwrap().input = false;
            let usage = o.usage.as_mut().unwrap();
            usage.cached_input_tokens = Some(2);
            usage.input_tokens = 999;
        }),
        (
            "missing primary output cannot hide a positive normalized total",
            |o| {
                o.usage_presence.as_mut().unwrap().output = false;
            },
        ),
        ("cache subtotal overflow", |o| {
            let usage = o.usage.as_mut().unwrap();
            usage.cached_input_tokens = Some(u64::MAX);
            usage.cache_write_input_tokens = Some(1);
        }),
        ("primary input below cache subset", |o| {
            o.usage.as_mut().unwrap().cached_input_tokens = Some(12);
        }),
        ("primary output below reasoning subset", |o| {
            o.usage.as_mut().unwrap().reasoning_tokens = 4;
        }),
        ("legacy missing evidence", |o| o.usage_presence = None),
        ("reported counters lost values", |o| o.usage = None),
        ("changed original model", |o| o.model = "foreign".into()),
        ("changed original purpose", |o| {
            o.purpose = ProviderRequestPurpose::Memory
        }),
    ];
    for (label, change) in changes {
        let Case {
            mut service,
            dispatch,
            issued,
            factory,
            store,
            member,
            mut root,
        } = case(BudgetAttemptPurpose::Execution).await;
        let provider = OpenAiCompatibleChatCompletionsProvider::without_auth(Transport {
            usage: serde_json::json!({"prompt_tokens":11,"completion_tokens":3}),
            calls: Arc::new(AtomicUsize::new(0)),
        })
        .unwrap()
        .with_provider_id("local-chat")
        .unwrap();
        let request = crate::integrations::agent::context::assemble_model_request(
            &dispatch.model_profile,
            provider.api_compatibility(),
            &dispatch.turn,
            dispatch.context.durable(),
        )
        .unwrap();
        let pending = PendingProviderWireObservations::default();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let observer = ProviderWireRequestObserver::new(&member.conversation, "%1", sender)
            .with_pending_observations(pending.clone());
        let observed = ObservedAsyncModelProvider::new(
            &provider,
            &observer,
            ProviderRequestPurpose::Execution,
        );
        let mut call = Box::pin(tokio::task::unconstrained(
            observed.send_request_async(&request),
        ));
        let original = tokio::select! {
            biased;
            _ = &mut call => panic!("original actor handoff must wait"),
            observation = receiver.recv() => observation.unwrap(),
        };
        pending.acknowledge(&original.request_id);
        call.await.unwrap();
        let mut invalid = original.clone();
        change(&mut invalid);
        let refused = factory.freeze(&invalid).is_err();
        let sticky = factory.freeze(&original).is_err();
        let cloned = BudgetObservationReceipts::new(issued.registered().clone());
        let shared_refusal = cloned.freeze(&original).is_err();
        let planning_sealed = cloned.plan(&original).is_err();
        let unchanged = rows(&store, "budget_receipts") == 0
            && rows(&store, "budget_admissions") == 1
            && store.budget_account(&member.root).unwrap().known == UsageTotals::default();
        service.terminate_all_pane_processes().unwrap();
        drop((
            observer, provider, cloned, factory, issued, dispatch, service,
        ));
        root.cleanup();
        assert!(refused, "{label}");
        assert!(
            sticky && shared_refusal && planning_sealed && unchanged,
            "{label}: refusal lost original custody"
        );
    }
}
