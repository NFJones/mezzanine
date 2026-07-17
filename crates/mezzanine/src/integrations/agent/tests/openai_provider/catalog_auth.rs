//! Openai Provider tests for catalog auth behavior.
//!
//! This bounded leaf owns the named behavioral scenarios.

use super::*;

#[test]
/// Verifies openai provider can be constructed from auth store secret reference.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn openai_provider_can_be_constructed_from_auth_store_secret_reference() {
    let root = std::env::temp_dir().join(format!("mez-agent-provider-auth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_api_key("default", "sk-provider-test", &credential_store)
        .unwrap();
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"model":"gpt-test","output_text":"ok"}"#.to_string(),
        },
    };

    let provider = openai_provider_from_auth_store_with_transport(&auth_store, transport).unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer sk-provider-test")
    );
    let metadata = std::fs::read_to_string(auth_store.paths().auth_file()).unwrap();
    assert!(!metadata.contains("sk-provider-test"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies that an API-key provider built from configuration expands
/// `base_url` before issuing requests. Without this regression coverage, a
/// configured value such as `https://api.openai.com/v1` can be treated as a
/// literal Responses endpoint, breaking normal requests while model listing
/// appears superficially valid.
fn openai_provider_from_auth_store_expands_configured_base_url() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-auth-base-url-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_api_key("default", "sk-provider-test", &credential_store)
        .unwrap();
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"model":"gpt-test","output_text":"ok"}"#.to_string(),
        },
    };

    let provider = openai_provider_from_auth_store_with_options(
        &auth_store,
        Some("https://api.openai.com/v1"),
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, OPENAI_RESPONSES_ENDPOINT);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies that a ChatGPT browser/device login is not treated as a direct API
/// key. ChatGPT credentials must go to the ChatGPT Codex backend and include
/// the account-id header that selects the authenticated account.
fn openai_provider_from_auth_store_routes_chatgpt_credentials_to_codex_backend() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-chatgpt-auth-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            OpenAiProviderCredential {
                api_key: "chatgpt-access-token".to_string(),
                refresh_token: Some("refresh-token".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: None,
                token_expires_at: Some("12345".to_string()),
            },
            &credential_store,
        )
        .unwrap();
    let request = assemble_model_request(
        &ModelProfile {
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: format!(
                "event: response.output_item.done\ndata: {}\n\nevent: response.completed\ndata: {}\n\n",
                serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "ok"}]
                    }
                }),
                serde_json::json!({
                    "type": "response.completed",
                    "response": {"id": "resp_1", "model": "gpt-test"}
                })
            ),
        },
    };

    let provider = openai_provider_from_auth_store_with_transport(&auth_store, transport).unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, CHATGPT_RESPONSES_ENDPOINT);
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer chatgpt-access-token")
    );
    assert_eq!(
        sent[0].headers.get("Accept").map(String::as_str),
        Some("text/event-stream")
    );
    assert_eq!(
        sent[0]
            .headers
            .get(CHATGPT_ACCOUNT_ID_HEADER)
            .map(String::as_str),
        Some("acct_123")
    );
    let request_body: serde_json::Value = serde_json::from_str(&sent[0].body).unwrap();
    assert_eq!(request_body["stream"], true);
    let metadata = std::fs::read_to_string(auth_store.paths().auth_file()).unwrap();
    assert!(metadata.contains("credential_kind = \"chatgpt\""));
    assert!(!metadata.contains("chatgpt-access-token"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies that `ModelProvider::list_models` for OpenAI issues an authenticated
/// GET request and normalizes the response into a model catalog with any
/// provider-reported quota usage. This is the provider-backed path consumed by
/// the agent `/model list` runtime command.
fn openai_provider_lists_models_through_authenticated_catalog_request() {
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: std::collections::BTreeMap::from([
                ("x-ratelimit-limit-requests".to_string(), "40".to_string()),
                (
                    "x-ratelimit-remaining-requests".to_string(),
                    "30".to_string(),
                ),
            ]),
            body: r#"{"data":[{"id":"gpt-5.5"}]}"#.to_string(),
        },
    };
    let provider = OpenAiResponsesProvider::new("sk-model-list", transport).unwrap();

    let catalog = provider.list_models().unwrap();

    assert_eq!(catalog.provider, "openai");
    assert_eq!(catalog.source, "provider");
    assert_eq!(catalog.models[0].id, "gpt-5.5");
    assert_eq!(
        catalog.reasoning_levels,
        vec!["low", "medium", "high", "xhigh"]
    );
    assert_eq!(catalog.quota_usage.len(), 1);
    assert_eq!(catalog.quota_usage[0].name, "requests");
    assert_eq!(catalog.quota_usage[0].used_percent_display(), "25.00%");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].method, "GET");
    assert_eq!(sent[0].url, OPENAI_MODELS_ENDPOINT);
    assert_eq!(
        sent[0].headers.get("Authorization").map(String::as_str),
        Some("Bearer sk-model-list")
    );
}

#[test]
/// Verifies that OpenAI model catalog requests include the documented
/// organization and project routing headers when configured. Multi-org and
/// project-scoped API keys depend on these headers for accurate model access,
/// usage accounting, and provider-reported rate-limit measurements.
fn openai_provider_model_catalog_uses_documented_accounting_headers() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-openai-routing-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_api_key("default", "sk-routed", &credential_store)
        .unwrap();
    let mut provider_options = std::collections::BTreeMap::new();
    provider_options.insert("organization_id".to_string(), "org_configured".to_string());
    provider_options.insert("project_id".to_string(), "proj_configured".to_string());
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"object":"list","data":[{"id":"gpt-routed","object":"model","created":1686935002,"owned_by":"openai"}]}"#
                .to_string(),
        },
    };
    let provider = openai_provider_from_auth_store_with_provider_options(
        &auth_store,
        Some("https://api.openai.com/v1"),
        &provider_options,
        120_000,
        transport,
    )
    .unwrap();

    let catalog = provider.list_models().unwrap();

    assert_eq!(catalog.models[0].id, "gpt-routed");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, OPENAI_MODELS_ENDPOINT);
    assert_eq!(
        sent[0]
            .headers
            .get("OpenAI-Organization")
            .map(String::as_str),
        Some("org_configured")
    );
    assert_eq!(
        sent[0].headers.get("OpenAI-Project").map(String::as_str),
        Some("proj_configured")
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies OpenAI rate-limit headers are normalized into stable percentage
/// measurements even when numeric header values contain common visual
/// separators. Provider headers are the documented live rate-limit source for
/// ordinary API-key requests.
fn openai_rate_limit_headers_allow_grouped_numeric_values() {
    let quotas = provider_quota_usage_from_headers(&std::collections::BTreeMap::from([
        (
            "X-RateLimit-Limit-Requests".to_string(),
            "1,000".to_string(),
        ),
        (
            "X-RateLimit-Remaining-Requests".to_string(),
            "750".to_string(),
        ),
        ("X-RateLimit-Reset-Requests".to_string(), "1s".to_string()),
    ]));

    assert_eq!(quotas.len(), 1);
    assert_eq!(quotas[0].name, "requests");
    assert_eq!(quotas[0].limit, 1000);
    assert_eq!(quotas[0].remaining, 750);
    assert_eq!(quotas[0].used_percent_display(), "25.00%");
    assert_eq!(quotas[0].reset.as_deref(), Some("1s"));
}

#[test]
/// Verifies configured OpenAI Responses-compatible providers can run without
/// stored auth metadata.
///
/// Local API servers such as LM Studio commonly accept unauthenticated
/// OpenAI-compatible requests. Missing metadata must therefore build a provider
/// that omits `Authorization` instead of failing before the HTTP request.
fn openai_responses_compatible_provider_omits_auth_when_metadata_is_absent() {
    let root = std::env::temp_dir().join(format!(
        "mez-agent-provider-no-auth-responses-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let auth_store = AuthStore::new(crate::security::auth::AuthPaths::under_config_root(&root));
    let request = assemble_model_request(
        &ModelProfile {
            provider: "lmstudio".to_string(),
            model: "local-model".to_string(),
            reasoning_profile: None,
            latency_preference: None,
            multimodal_required: false,
            provider_options: std::collections::BTreeMap::new(),
            safety_tier: None,
        },
        &turn(),
        &AgentContext::new(vec![ContextBlock {
            source: ContextSourceKind::UserInstruction,
            placement: mez_agent::ContextPlacement::EphemeralTail,
            label: "user".to_string(),
            content: "hello".to_string(),
        }])
        .unwrap(),
    )
    .unwrap();
    let transport = FakeProviderHttpTransport {
        requests: RefCell::new(Vec::new()),
        response: ProviderHttpResponse {
            status_code: 200,
            headers: Default::default(),
            body: r#"{"model":"local-model","output_text":"ok"}"#.to_string(),
        },
    };

    let provider = openai_responses_provider_from_auth_store_with_provider_options(
        &auth_store,
        "lmstudio",
        Some("http://localhost:1234/v1"),
        &std::collections::BTreeMap::new(),
        120_000,
        transport,
    )
    .unwrap();
    let response = provider.send_request(&request).unwrap();

    assert_eq!(response.provider, "lmstudio");
    assert_eq!(response.raw_text, "ok");
    let sent = provider.transport.requests.borrow();
    assert_eq!(sent[0].url, "http://localhost:1234/v1/responses");
    assert_eq!(sent[0].headers.get("Authorization"), None);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
/// Verifies that configured OpenAI provider URLs are interpreted as API base
/// URLs, not as literal request endpoints. This protects the config contract:
/// `https://api.openai.com/v1` must drive model requests through `/models` and
/// normal generation requests through `/responses`.
fn openai_responses_endpoint_derives_from_configured_base_url() {
    assert_eq!(
        openai_responses_endpoint_for_base_url("https://api.openai.com/v1").unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
    assert_eq!(
        openai_responses_endpoint_for_base_url("https://api.openai.com/v1/").unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
    assert_eq!(
        openai_responses_endpoint_for_base_url(OPENAI_RESPONSES_ENDPOINT).unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
    assert_eq!(
        openai_responses_endpoint_for_base_url(OPENAI_MODELS_ENDPOINT).unwrap(),
        OPENAI_RESPONSES_ENDPOINT
    );
}
