//! Immutable project accounting capture over worker-qualified mappings.
//!
//! Trust is evaluated at request freeze, never inferred from mapping possession.
//! Claim provenance survives cwd/revocation while new work uses current state.

use super::*;
use crate::storage::token_usage::{AccountingOrigin, TokenUsageStore};

/// Managed provider continuations must observe standalone persisted revocation
/// after mapping preparation. The earlier issued dispatch keeps its origin;
/// the fresh dispatch must not charge the revoked project. No provider I/O runs.
#[tokio::test]
async fn runtime_accounting_origin_managed_dispatch_observes_persisted_revocation() {
    let base = temp_root("accounting-managed-revocation");
    let project = base.join("project");
    fs::create_dir_all(&project).unwrap();
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "accounting-managed".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode = \"pane\"\ndefault_provider = \"openai\"\ndefault_model_profile = \"accounting-test\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-test\"]\n[model_profiles.accounting-test]\nprovider = \"openai\"\nmodel = \"gpt-test\"\n".to_string(),
    }]).unwrap();
    let auth = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(
        &base.join("auth"),
    ));
    let credentials = auth.file_credential_store("openai").unwrap();
    auth.login_openai_api_key("accounting-test", "sk-accounting-fixture", &credentials)
        .unwrap();
    service.set_auth_store(auth);
    service.set_token_usage_store(TokenUsageStore::new(base.join("usage.sqlite")));
    let trust_path = base.join("trust.sqlite");
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(project.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust.save_to_file(&trust_path).unwrap();
    service.set_project_trust_store(trust.clone(), Some(trust_path.clone()));
    service.set_pane_current_working_directory("%1".to_string(), project.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let signature = mez_agent::EnvironmentSignature::new(
        "linux",
        "x86_64",
        None,
        "test-host",
        "test-user",
        None,
        "/bin/sh",
        mez_agent::ShellClassification::PosixSh,
        None,
        Some("/usr/bin:/bin".to_string()),
        project.to_string_lossy(),
        None,
        false,
        None,
        Vec::new(),
    )
    .unwrap();
    service.set_pane_environment_signature_for_tests("%1", signature);
    mark_test_pane_ready(&mut service, "%1");
    let started = service
        .start_agent_prompt_turn("%1", "freeze managed attribution")
        .unwrap();
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let request = service
        .primary_path_resolution_request("%1")
        .unwrap()
        .unwrap();
    let command = mez_agent::shell::pane_path_resolution_command(
        &request,
        mez_agent::ShellClassification::PosixSh,
    )
    .unwrap();
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(&project)
        .output()
        .unwrap();
    assert!(output.status.success());
    let key = service.path_resolution_cache_key("%1", &request).unwrap();
    service
        .observe_path_resolution_transaction_end(
            "accounting-managed-resolution",
            "%1",
            0,
            key,
            &String::from_utf8(output.stdout).unwrap(),
            false,
        )
        .unwrap();
    let agent = AgentId::opaque("agent-%1".to_string()).unwrap();
    let first = service
        .claim_configured_agent_provider_task(&agent, &started.turn_id)
        .unwrap()
        .expect("managed request should be claimable");
    assert!(matches!(
        first.accounting_origin,
        AccountingOrigin::Project(_)
    ));
    service
        .record_claimed_agent_provider_task(&first, 1, 30_000)
        .unwrap();
    trust.decide(project, TrustDecision::Revoked, None).unwrap();
    trust.save_to_file(&trust_path).unwrap();
    service.queue_agent_provider_task(started.turn_id.clone());
    let fresh = service
        .claim_configured_agent_provider_task(&agent, &started.turn_id)
        .unwrap()
        .expect("revoked root should not block policy-only provider work");
    assert_eq!(fresh.accounting_origin, AccountingOrigin::Unattributed);
    assert_eq!(
        service.claimed_accounting_origin_for_tests(&started.turn_id),
        Some(&first.accounting_origin)
    );
    fs::remove_dir_all(base).unwrap();
}

/// A worker prepared from a prior trust inventory cannot replace the current
/// mapping cache after revocation. New capture remains unattributed, while the
/// worker may retain historical metadata without granting project authority.
#[tokio::test]
async fn runtime_accounting_origin_rejects_stale_preparation_inventory() {
    let base = temp_root("accounting-stale-inventory");
    fs::create_dir_all(&base).unwrap();
    let mut service = test_runtime_service();
    service.set_token_usage_store(TokenUsageStore::new(base.join("usage.sqlite")));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(base.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust.clone(), None);
    service.set_pane_current_working_directory("%1".to_string(), base.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "prepare mapping only")
        .unwrap();
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    trust
        .decide(base.clone(), TrustDecision::Revoked, None)
        .unwrap();
    service.set_project_trust_store(trust, None);
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    assert!(service.persistence.accounting_projects().is_none());
    assert_eq!(
        service.capture_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    fs::remove_dir_all(base).unwrap();
}

/// A real configured provider dispatch captures project attribution before any
/// provider I/O. The claim retains that origin after cwd changes, independently
/// of its execution generation; no paid request is needed to test this boundary.
#[tokio::test]
async fn runtime_accounting_origin_production_dispatch_retains_frozen_project() {
    let base = temp_root("accounting-dispatch-freeze");
    let a = base.join("a");
    let b = base.join("b");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    let mut service = test_runtime_service();
    service.replace_config_layers(vec![ConfigLayer {
        name: "accounting-dispatch".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\nshell_mode = \"native\"\ndefault_provider = \"openai\"\ndefault_model_profile = \"accounting-test\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-test\"]\n[model_profiles.accounting-test]\nprovider = \"openai\"\nmodel = \"gpt-test\"\n".to_string(),
    }]).unwrap();
    let auth = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(
        &base.join("auth"),
    ));
    let credentials = auth.file_credential_store("openai").unwrap();
    auth.login_openai_api_key("accounting-test", "sk-accounting-fixture", &credentials)
        .unwrap();
    service.set_auth_store(auth);
    service.set_token_usage_store(TokenUsageStore::new(base.join("usage.sqlite")));
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(a.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust
        .decide(b.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust, None);
    service
        .start_initial_pane_process_with_start_directory(None, &a)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "freeze attribution only")
        .unwrap();
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let dispatch = service
        .claim_configured_agent_provider_task(
            &AgentId::opaque("agent-%1".to_string()).unwrap(),
            &started.turn_id,
        )
        .unwrap()
        .expect("configured native dispatch should be claimable");
    let frozen = dispatch.accounting_origin.clone();
    assert!(matches!(frozen, AccountingOrigin::Project(_)));
    service.set_pane_current_working_directory("%1".to_string(), b);
    assert_ne!(service.capture_accounting_origin_for_pane("%1"), frozen);
    service
        .record_claimed_agent_provider_task(&dispatch, 7, 30_000)
        .unwrap();
    assert_eq!(
        service.claimed_accounting_origin_for_tests(&started.turn_id),
        Some(&frozen)
    );
    service.terminate_all_pane_processes().unwrap();
    fs::remove_dir_all(base).unwrap();
}

/// Production preparation qualifies inventory off actor ownership. A captured
/// claim remains in project A after cwd moves to B; a fresh origin uses B, and
/// deepest rejection/revocation withholds attribution without ancestor fallback.
#[tokio::test]
async fn runtime_accounting_origin_freezes_claim_and_rechecks_current_trust() {
    let base = temp_root("accounting-origin-freeze");
    let a = base.join("a");
    let b = base.join("b");
    let nested = a.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&b).unwrap();
    let mut service = test_runtime_service();
    let store = TokenUsageStore::new(base.join("usage.sqlite"));
    service.set_token_usage_store(store);
    let mut trust = ProjectTrustStore::default();
    trust
        .decide(a.clone(), TrustDecision::Trusted, None)
        .unwrap();
    trust
        .decide(b.clone(), TrustDecision::Trusted, None)
        .unwrap();
    service.set_project_trust_store(trust.clone(), None);
    service.set_pane_current_working_directory("%1".to_string(), a.clone());
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    let started = service
        .start_agent_prompt_turn("%1", "capture accounting origin")
        .unwrap();
    assert_eq!(
        service.capture_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    let preparation = service
        .prepare_agent_provider_work(&started.turn_id)
        .unwrap();
    let outcome = RuntimeSessionService::execute_agent_provider_preparation(preparation).await;
    service.apply_agent_provider_preparation(outcome).unwrap();
    let origin_a = service.capture_accounting_origin_for_pane("%1");
    assert!(matches!(origin_a, AccountingOrigin::Project(_)));
    service
        .record_claimed_agent_provider_context_for_tests(&started.turn_id, 0)
        .unwrap();
    service.set_pane_current_working_directory("%1".to_string(), b.clone());
    let origin_b = service.capture_accounting_origin_for_pane("%1");
    assert!(matches!(origin_b, AccountingOrigin::Project(_)));
    assert_ne!(origin_a, origin_b);
    assert_eq!(
        service.claimed_accounting_origin_for_tests(&started.turn_id),
        Some(&origin_a)
    );
    for decision in [TrustDecision::Rejected, TrustDecision::Revoked] {
        trust.decide(nested.clone(), decision, None).unwrap();
        service.set_project_trust_store(trust.clone(), None);
        service.set_pane_current_working_directory("%1".to_string(), nested.clone());
        assert_eq!(
            service.capture_accounting_origin_for_pane("%1"),
            AccountingOrigin::Unattributed
        );
        assert_eq!(
            service.claimed_accounting_origin_for_tests(&started.turn_id),
            Some(&origin_a)
        );
    }
    service.set_project_trust_store(ProjectTrustStore::default(), None);
    assert_eq!(
        service.capture_accounting_origin_for_pane("%1"),
        AccountingOrigin::Unattributed
    );
    fs::remove_dir_all(base).unwrap();
}
