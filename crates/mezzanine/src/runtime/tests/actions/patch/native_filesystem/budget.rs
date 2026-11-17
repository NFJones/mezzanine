//! Real native membership and actor filesystem admission under writer ordering.
//! These tests retain the original producer and retire it before fixture removal.

use super::*;
use crate::storage::token_usage::TokenUsageStore;
use crate::storage::token_usage::budget::policy_projection::{
    BudgetConfigPolicy, BudgetPolicyPublication,
};
use futures_util::FutureExt;
use mez_agent::token_budget::{BudgetLimits, BudgetReceiptId};
use std::time::Duration;

/// Only after actual producer/actor joins may the original binding lease be
/// dropped, then its private lock inode and fixture directory removed.
fn cleanup(
    mut fixture: Fixture,
    producer: crate::runtime::native_filesystem::NativeFilesystemProducer,
    attempt: crate::runtime::agent::native_filesystem::NativeFilesystemAttempt,
    store: TokenUsageStore,
) {
    let root = fixture.root.clone();
    fixture.service.terminate_all_pane_processes().unwrap();
    drop((producer, attempt, store));
    let service = std::mem::replace(&mut fixture.service, test_runtime_service());
    drop(service);
    fixture.retain_root = false;
    drop(fixture);
    assert!(
        !root.exists(),
        "owned budget fixture was not removed: {}",
        root.display()
    );
}

async fn policy(store: &TokenUsageStore, limits: BudgetLimits) {
    let store = store.clone();
    tokio::task::spawn_blocking(move || {
        let projection = store.budget_policy_projection().unwrap();
        let publication = BudgetPolicyPublication {
            namespace: projection.namespace,
            id: BudgetReceiptId::new(crate::storage::token_usage::new_token_usage_event_id())
                .unwrap(),
            expected_revision: projection.revision,
            previous_digest: projection
                .committed
                .as_ref()
                .map_or_else(|| "a".repeat(64), |p| p.digest.clone()),
            proposed: BudgetConfigPolicy {
                source: "primary".into(),
                digest: format!("{:064x}", projection.revision + 1),
                limits,
            },
            supersedes: None,
        };
        store.begin_budget_policy_publication(&publication).unwrap();
        store
            .reconcile_budget_policy_publication(&publication, &publication.proposed.digest)
            .unwrap();
    })
    .await
    .unwrap();
}

async fn prepared() -> (Fixture, TokenUsageStore) {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::with_transcript_store("history");
    fixture.retain_root = true;
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.service.replace_config_layers(vec![ConfigLayer {
        name: "budget-filesystem".into(), path: None, format: ConfigFormat::Toml,
        scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode=\"native\"\ndefault_provider=\"openai\"\ndefault_model_profile=\"budget-test\"\n[permissions]\nsandbox=\"policy-only\"\n[providers.openai]\nkind=\"openai\"\nmodels=[\"gpt-test\"]\n[model_profiles.budget-test]\nprovider=\"openai\"\nmodel=\"gpt-test\"\n".into(),
    }]).unwrap();
    let auth = crate::security::auth::AuthStore::new(
        crate::security::auth::AuthPaths::under_config_root(&fixture.root.join("auth")),
    );
    let credentials = auth.file_credential_store("openai").unwrap();
    auth.login_openai_api_key("budget-test", "sk-budget-offline-fixture", &credentials)
        .unwrap();
    fixture.service.set_auth_store(auth);
    let store = TokenUsageStore::new(fixture.root.join("usage.sqlite"));
    store
        .initialize(crate::runtime::current_unix_seconds())
        .unwrap();
    fixture.service.set_token_usage_store(store.clone());
    fixture.service.enable_runtime_budget_membership_for_tests();
    let work = fixture
        .service
        .prepare_agent_provider_work(&fixture.turn.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(work).await;
    fixture
        .service
        .apply_agent_provider_preparation(outcome)
        .unwrap();
    assert!(
        fixture
            .service
            .runtime_budget_membership_is_ready(&fixture.turn)
    );
    policy(&store, BudgetLimits::default()).await;
    // Reinstall this fixture's explicit path authority after config adoption.
    let permissions = crate::runtime::config::runtime_configured_permissions_from_config(
        &serde_json::json!({"permissions":{"read_scopes":[fixture.root],"write_scopes":[fixture.root]}}),
    ).unwrap();
    fixture
        .service
        .integration
        .replace_configured_permissions(permissions);
    fixture
        .service
        .permission_policy_mut()
        .set_approval_bypass(true);
    (fixture, store)
}

#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_cannot_bypass_existing_accounting_writer() {
    let (mut fixture, store) = prepared().await;
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    let actor = super::async_producer::ActorFixture::new(fixture);
    let writer = rusqlite::Connection::open(store.path()).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let issued = actor
        .handle
        .claim_native_filesystem_producer(producer.clone())
        .await;
    writer.execute_batch("ROLLBACK").unwrap();
    drop(writer);
    let recovered = if issued.as_ref().is_ok_and(|issued| !issued) {
        crate::host::async_runtime::native_filesystem::claim_and_start_native_filesystem_producer(
            actor.handle.clone(),
            &producer,
        )
        .await
        .unwrap()
    } else {
        false
    };
    let outcome = if recovered {
        crate::host::async_runtime::native_filesystem::project_joined_native_filesystem_facts(
            &actor.handle,
            &producer,
        )
        .await
        .unwrap()
    } else {
        None
    };
    // Positive original retirement precedes the regression verdict, including
    // the pre-fix path that wrongly installs a never-started dispatch.
    producer.close().unwrap();
    producer.retire().await.unwrap();
    let fixture = actor.finish().await;
    let contents = fs::read(fixture.root.join("file")).ok();
    cleanup(fixture, producer, attempt, store);
    assert!(
        !issued.unwrap(),
        "filesystem claim bypassed the original accounting/policy writer"
    );
    assert!(recovered);
    assert_eq!(contents.as_deref(), Some(b"native data\n".as_slice()));
    assert_eq!(
        outcome.unwrap().result.certainty,
        NativeEffectCertainty::Confirmed
    );
}

/// A genuine earlier writer claim may cross into actor issuance after policy
/// changes; an equal-token but nonenrolled producer must never receive it.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_precedes_later_policy_but_not_producer_identity() {
    for replaced in [false, true] {
        let (mut fixture, store) = prepared().await;
        let attempt = fixture.queue();
        let producer = attempt.root_producer().unwrap();
        let crate::runtime::RuntimeBudgetFilesystemClaimPreparation::Work(work) = fixture
            .service
            .prepare_budget_filesystem_claim(&producer)
            .unwrap()
        else {
            panic!("original queued filesystem offer unavailable")
        };
        let actor = super::async_producer::ActorFixture::new(fixture);
        assert!(producer.start_budget_claim(work).await.unwrap());
        let claim = producer.observe_budget_claim().await.unwrap().unwrap();
        let issued = if replaced {
            let lookalike =
                crate::runtime::native_filesystem::NativeFilesystemProducer::new(attempt.clone());
            let refused = !actor
                .handle
                .finish_budget_filesystem_claim(lookalike.clone(), claim)
                .await
                .unwrap();
            // This unstarted alias has no actual worker or dispatch. Do not
            // cancel its shared attempt while the genuine producer can recover.
            drop(lookalike);
            assert!(
                refused,
                "equal attempt text transferred to another producer allocation"
            );
            actor
                .handle
                .claim_native_filesystem_producer(producer.clone())
                .await
                .unwrap()
        } else {
            policy(
                &store,
                BudgetLimits {
                    input: mez_agent::token_budget::TokenLimit::Limited(0),
                    output: mez_agent::token_budget::TokenLimit::Limited(0),
                },
            )
            .await;
            actor
                .handle
                .finish_budget_filesystem_claim(producer.clone(), claim)
                .await
                .unwrap()
        };
        let started =
            crate::host::async_runtime::native_filesystem::start_native_filesystem_producer(
                actor.handle.clone(),
                &producer,
            )
            .await
            .unwrap();
        let outcome =
            crate::host::async_runtime::native_filesystem::project_joined_native_filesystem_facts(
                &actor.handle,
                &producer,
            )
            .await
            .unwrap()
            .unwrap();
        producer.retire().await.unwrap();
        let fixture = actor.finish().await;
        let contents = fs::read(fixture.root.join("file")).unwrap();
        cleanup(fixture, producer, attempt, store);
        assert!(issued && started);
        assert_eq!(contents, b"native data\n");
        assert_eq!(outcome.result.certainty, NativeEffectCertainty::Confirmed);
    }
}

/// Numeric inhibition and local capture/reader failure leave the original
/// queued producer recoverable; none may invent a terminal patch result.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_local_holds_recover_original_patch() {
    for fault in 0..3 {
        let (mut fixture, store) = prepared().await;
        let root = fixture.root.clone();
        let attempt = fixture.queue();
        let producer = attempt.root_producer().unwrap();
        if fault == 0 {
            policy(
                &store,
                BudgetLimits {
                    input: mez_agent::token_budget::TokenLimit::Limited(0),
                    output: mez_agent::token_budget::TokenLimit::Unlimited,
                },
            )
            .await;
        } else {
            fixture
                .service
                .inject_budget_activity_fault(fixture.turn.turn_id.clone(), fault == 1);
        }
        let actor = super::async_producer::ActorFixture::new(fixture);
        let exercised = std::panic::AssertUnwindSafe(async {
            let issued = actor.handle.claim_native_filesystem_producer(producer.clone()).await.unwrap();
            let closed = producer.is_closed().unwrap();
            let changed = root.join("file").exists();
            if fault == 0 { policy(&store, BudgetLimits::default()).await; }
            let recovered = crate::host::async_runtime::native_filesystem::claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer).await.unwrap();
            let outcome = crate::host::async_runtime::native_filesystem::project_joined_native_filesystem_facts(&actor.handle, &producer).await.unwrap().unwrap();
            (issued, closed, changed, recovered, outcome.result.certainty)
        }).catch_unwind().await;
        producer.retire().await.unwrap();
        let fixture = actor.finish().await;
        let contents = fs::read(root.join("file")).ok();
        cleanup(fixture, producer, attempt, store);
        let (issued, closed, changed, recovered, certainty) = exercised.unwrap();
        assert!(
            !issued && !closed && !changed && recovered,
            "local fault {fault} lost the original pending producer"
        );
        assert_eq!(certainty, NativeEffectCertainty::Confirmed);
        assert_eq!(contents.as_deref(), Some(b"native data\n".as_slice()));
    }
}

/// An unconsumed reservation can be withdrawn, but a duplicate preparation
/// cannot supersede it or allocate a second claim for the same original patch.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_unconsumed_work_and_result_withdraw() {
    for complete in [false, true] {
        let (mut fixture, store) = prepared().await;
        let root = fixture.root.clone();
        let attempt = fixture.queue();
        let producer = attempt.root_producer().unwrap();
        let crate::runtime::RuntimeBudgetFilesystemClaimPreparation::Work(work) = fixture
            .service
            .prepare_budget_filesystem_claim(&producer)
            .unwrap()
        else {
            panic!("original budget work unavailable")
        };
        let duplicate_refused = matches!(
            fixture
                .service
                .prepare_budget_filesystem_claim(&producer)
                .unwrap(),
            crate::runtime::RuntimeBudgetFilesystemClaimPreparation::Refused
        );
        let actor = super::async_producer::ActorFixture::new(fixture);
        let exercised = std::panic::AssertUnwindSafe(async {
            if complete {
                assert!(producer.start_budget_claim(work).await.unwrap());
                drop(producer.observe_budget_claim().await.unwrap().unwrap());
            } else { drop(work); }
            let issued = crate::host::async_runtime::native_filesystem::claim_and_start_native_filesystem_producer(actor.handle.clone(), &producer).await.unwrap();
            let outcome = crate::host::async_runtime::native_filesystem::project_joined_native_filesystem_facts(&actor.handle, &producer).await.unwrap().unwrap();
            (issued, outcome.result.certainty)
        }).catch_unwind().await;
        producer.retire().await.unwrap();
        let fixture = actor.finish().await;
        let contents = fs::read(root.join("file")).ok();
        cleanup(fixture, producer, attempt, store);
        let (issued, certainty) = exercised.unwrap();
        assert!(duplicate_refused && issued);
        assert_eq!(certainty, NativeEffectCertainty::Confirmed);
        assert_eq!(contents.as_deref(), Some(b"native data\n".as_slice()));
    }
}

/// A borrowed observer/retirement future can disappear while the physical
/// reader runs. The retained owner must still join it before a refusal proof.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_reader_observer_cancellation_keeps_retirement_custody() {
    let (mut fixture, store) = prepared().await;
    let root = fixture.root.clone();
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    let crate::runtime::RuntimeBudgetFilesystemClaimPreparation::Work(mut work) = fixture
        .service
        .prepare_budget_filesystem_claim(&producer)
        .unwrap()
    else {
        panic!("original budget work unavailable")
    };
    let (entered, release) = work.gate_native_writer_for_tests();
    let actor = super::async_producer::ActorFixture::new(fixture);
    let started = producer.start_budget_claim(work).await.unwrap();
    let entered = tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .is_ok();
    let mut observer = Box::pin(producer.observe_budget_claim());
    let observer_pending = futures_util::poll!(&mut observer).is_pending();
    drop(observer);
    let retained = producer.has_budget_claim().await;
    let closed = producer.close();
    let early_refusal = actor
        .handle
        .refuse_native_filesystem_producer(producer.clone())
        .await;
    let mut retirement = Box::pin(producer.retire());
    let retirement_pending = futures_util::poll!(&mut retirement).is_pending();
    drop(retirement);
    let released = release.send(()).is_ok();
    let retired = producer.retire().await;
    let no_reader = !producer.has_budget_claim().await;
    let final_refusal = actor
        .handle
        .refuse_native_filesystem_producer(producer.clone())
        .await;
    let fixture = actor.finish().await;
    let changed = root.join("file").exists();
    cleanup(fixture, producer, attempt, store);
    assert!(
        started
            && entered
            && observer_pending
            && retained
            && retirement_pending
            && released
            && no_reader
    );
    closed.unwrap();
    retired.unwrap();
    assert!(
        early_refusal.unwrap().is_none(),
        "unjoined reader manufactured positive refusal custody"
    );
    assert!(final_refusal.unwrap().is_some());
    assert!(!changed);
}

/// A numeric hold cannot hide the already elapsed original action deadline.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_preserves_original_deadline_rejection() {
    let (mut fixture, store) = prepared().await;
    let root = fixture.root.clone();
    let attempt = fixture.queue();
    let producer = attempt.root_producer().unwrap();
    policy(
        &store,
        BudgetLimits {
            input: mez_agent::token_budget::TokenLimit::Limited(0),
            output: mez_agent::token_budget::TokenLimit::Unlimited,
        },
    )
    .await;
    fixture
        .service
        .expire_native_filesystem_deadline_for_tests(&attempt);
    let actor = super::async_producer::ActorFixture::new(fixture);
    let issued = actor
        .handle
        .claim_native_filesystem_producer(producer.clone())
        .await;
    producer.retire().await.unwrap();
    let refusal = actor
        .handle
        .refuse_native_filesystem_producer(producer.clone())
        .await;
    let fixture = actor.finish().await;
    let changed = root.join("file").exists();
    let rejected = refusal.as_ref().is_ok_and(|proof| {
        proof
            .as_ref()
            .is_some_and(|proof| !proof.cancelled() && proof.cause().message().contains("deadline"))
    });
    drop(refusal);
    cleanup(fixture, producer, attempt, store);
    assert!(issued.is_err() && rejected && !changed);
}

/// A lost prepare reply withdraws only unstarted authorization; a lost finish
/// reply cannot lose an already installed original dispatch or invent a join.
#[tokio::test(flavor = "current_thread")]
async fn native_filesystem_budget_claim_lost_actor_replies_retain_original_custody() {
    use crate::host::async_runtime::{AsyncRuntimeActorConfig, AsyncRuntimeSessionActor};
    for finish in [false, true] {
        let (mut fixture, store) = prepared().await;
        let root = fixture.root.clone();
        let attempt = fixture.queue();
        let producer = attempt.root_producer().unwrap();
        let claim = if finish {
            let crate::runtime::RuntimeBudgetFilesystemClaimPreparation::Work(work) = fixture
                .service
                .prepare_budget_filesystem_claim(&producer)
                .unwrap()
            else {
                panic!("original budget work unavailable")
            };
            assert!(producer.start_budget_claim(work).await.unwrap());
            producer.observe_budget_claim().await.unwrap()
        } else {
            None
        };
        let service = std::mem::replace(&mut fixture.service, test_runtime_service());
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let pending = if let Some(claim) = claim {
            let mut request =
                Box::pin(handle.finish_budget_filesystem_claim(producer.clone(), claim));
            let pending = futures_util::poll!(&mut request).is_pending();
            drop(request);
            pending
        } else {
            let mut request = Box::pin(handle.claim_native_filesystem_producer(producer.clone()));
            let pending = futures_util::poll!(&mut request).is_pending();
            drop(request);
            pending
        };
        let actor = tokio::spawn(actor.run());
        let exercised = std::panic::AssertUnwindSafe(async {
            // This actual subsequent request is an actor-queue adoption barrier.
            let recovered = handle.recover_native_filesystem_producers().await.unwrap();
            assert_eq!(recovered.len(), 1);
            if finish { assert!(producer.is_claimed().unwrap()); }
            else {
                assert!(crate::host::async_runtime::native_filesystem::claim_and_start_native_filesystem_producer(handle.clone(), &producer).await.unwrap());
                assert!(crate::host::async_runtime::native_filesystem::project_joined_native_filesystem_facts(&handle, &producer).await.unwrap().is_some());
            }
        }).catch_unwind().await;
        let outcome = producer.retire().await.unwrap().unwrap();
        handle
            .retain_native_filesystem_outcome(attempt.clone(), Arc::clone(&outcome))
            .await
            .unwrap();
        handle.shutdown().await.unwrap();
        fixture.service = actor.await.unwrap().service;
        let contents = fs::read(root.join("file")).ok();
        let certainty = outcome.result.certainty;
        drop(outcome);
        cleanup(fixture, producer, attempt, store);
        exercised.unwrap();
        assert!(pending);
        if finish {
            assert_eq!(certainty, NativeEffectCertainty::Unexecuted);
            assert!(contents.is_none());
        } else {
            assert_eq!(certainty, NativeEffectCertainty::Confirmed);
            assert_eq!(contents.as_deref(), Some(b"native data\n".as_slice()));
        }
    }
}
