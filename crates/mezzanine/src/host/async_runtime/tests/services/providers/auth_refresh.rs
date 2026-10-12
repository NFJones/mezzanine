//! Provider-refresh failures must settle only their turn, never the service.

use super::super::super::*;
use crate::security::auth::{AuthPaths, AuthStore, OpenAiProviderCredential};

/// Exercises real preparation, actor claim and provider-service polling with
/// an expired credential whose refresh secret has disappeared. No provider
/// request may dispatch; another terminal command must remain available and
/// the turn must not remain pending after its diagnostic is delivered.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_refresh_missing_secret_keeps_session_running() {
    exercise_refresh_containment(true).await;
}

/// Exercises HTTP invalid_grant through the actual provider preparation and
/// polling pipeline, then replaces credentials while the actor remains live.
/// Repeated requests must not reuse the rejected refresh token; another
/// configured provider and terminal commands must remain independently usable.
#[tokio::test(flavor = "current_thread")]
async fn async_provider_refresh_rejection_keeps_session_running_and_recovers() {
    exercise_refresh_containment(false).await;
}

/// Runs both missing-secret and HTTP-rejection cases against isolated stores.
async fn exercise_refresh_containment(missing: bool) {
    let root = std::env::temp_dir().join(format!(
        "mez-refresh-containment-{missing}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = requests.clone();
    let endpoint = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 8192];
            let _ = stream.read(&mut bytes).await.unwrap();
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let body = r#"{"error":"invalid_grant","error_description":"synthetic-refresh synthetic-access private-store-reference"}"#;
            stream.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let auth = AuthStore::new(AuthPaths::under_config_root(&root)).with_refresh_issuer(issuer);
    let store = auth.file_credential_store("openai").unwrap();
    let metadata = auth
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "synthetic-access".to_string(),
                refresh_token: Some("synthetic-refresh".to_string()),
                account_id: None,
                organization_id: None,
                token_expires_at: Some("1".to_string()),
            },
            &store,
        )
        .unwrap();
    if missing {
        std::fs::remove_file(
            metadata
                .refresh_credential_store_ref
                .unwrap()
                .strip_prefix("file:")
                .unwrap(),
        )
        .unwrap();
    }
    let mut service = test_service();
    service.set_auth_store(auth.clone());
    service.replace_config_layers(vec![ConfigLayer {
        name: "refresh-containment".to_string(), path: None,
        format: ConfigFormat::Toml, scope: ConfigScope::Primary, trusted: true,
        text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\nshell_mode = \"pane\"\n[permissions]\nsandbox = \"policy-only\"\n[providers.openai]\nkind = \"openai\"\nmodels = [\"fixture\"]\ndefault_model = \"fixture\"\n[providers.other]\nkind = \"openai\"\nmodels = [\"fixture\"]\ndefault_model = \"fixture\"\n[model_profiles.default]\nprovider = \"openai\"\nmodel = \"fixture\"\n[model_profiles.other]\nprovider = \"other\"\nmodel = \"fixture\"\n".to_string(),
    }]).unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();
    service
        .execute_agent_shell_command(&primary, "exercise expired credentials")
        .unwrap();
    let task = service.pending_agent_provider_tasks()[0].clone();
    let turn = task.turn_id.clone();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let client = async {
        handle
            .queue_runtime_side_effects(vec![RuntimeSideEffect::DispatchAgentProvider {
                agent_id: AgentId::opaque(task.agent_id).unwrap(),
                turn_id: task.turn_id,
            }])
            .await
            .unwrap();
        let report = run_async_agent_provider_service(
            &handle,
            AsyncAgentProviderServiceConfig::new(1).unwrap(),
            |polls, _| polls >= 1,
        )
        .await
        .unwrap();
        assert_eq!(report.executions, 0);
        assert!(
            handle
                .pending_agent_provider_tasks()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!handle.agent_turn_is_running(&turn).await.unwrap());
        assert_eq!(
            requests.load(std::sync::atomic::Ordering::SeqCst),
            usize::from(!missing)
        );
        if !missing {
            let error = auth
                .refresh_provider_credential_if_needed_with_leeway_async("openai", 0)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
            assert!(error.message().contains("mez auth login --provider openai"));
            for secret in [
                "synthetic-refresh",
                "synthetic-access",
                "private-store-reference",
            ] {
                assert!(!error.to_string().contains(secret));
            }
            assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
        // An API-key provider must claim normally despite the invalid unused
        // OpenAI profile. Claim only: never send a paid or real HTTP request.
        auth.login_with_api_key("other", "other", "synthetic-other", &store)
            .unwrap();
        handle
            .execute_agent_shell_command(primary.clone(), "/model other".to_string())
            .await
            .unwrap();
        handle
            .execute_agent_shell_command(primary.clone(), "other provider request".to_string())
            .await
            .unwrap();
        let other = handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .remove(0);
        let dispatch = handle
            .claim_configured_agent_provider_task(
                AgentId::opaque(other.agent_id).unwrap(),
                other.turn_id.clone(),
            )
            .await
            .unwrap();
        assert!(
            dispatch.is_some(),
            "unused rejected profile must not poison another provider"
        );
        handle
            .execute_agent_shell_command(primary.clone(), "/stop".to_string())
            .await
            .unwrap();
        // Explicit login updates the shared on-disk credentials in the same
        // session. A new request can claim again without replaying the old turn.
        auth.login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "replacement-access".to_string(),
                refresh_token: Some("replacement-refresh".to_string()),
                account_id: Some("synthetic-account".to_string()),
                organization_id: None,
                token_expires_at: Some("4102444800".to_string()),
            },
            &store,
        )
        .unwrap();
        handle
            .execute_agent_shell_command(primary.clone(), "/model default".to_string())
            .await
            .unwrap();
        handle
            .execute_agent_shell_command(primary.clone(), "request after login".to_string())
            .await
            .unwrap();
        let next = handle
            .pending_agent_provider_tasks()
            .await
            .unwrap()
            .remove(0);
        assert!(
            handle
                .claim_configured_agent_provider_task(
                    AgentId::opaque(next.agent_id).unwrap(),
                    next.turn_id
                )
                .await
                .unwrap()
                .is_some()
        );
        handle
            .execute_agent_shell_command(primary.clone(), "/stop".to_string())
            .await
            .unwrap();
        let output = handle
            .execute_terminal_command(primary, "new-window responsive".to_string())
            .await
            .unwrap();
        assert!(output.contains("window"), "{output}");
        assert_eq!(
            handle.shutdown().await.unwrap(),
            RuntimeLifecycleState::Running
        );
    };
    let ((), mut exit) = tokio::join!(client, actor.run());
    assert_eq!(exit.service.session().windows().len(), 2);
    exit.service.terminate_all_pane_processes().unwrap();
    endpoint.abort();
    let _ = std::fs::remove_dir_all(root);
}
