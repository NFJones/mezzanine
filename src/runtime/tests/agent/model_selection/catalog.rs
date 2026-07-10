//! Runtime tests for agent model_selection catalog behavior.

use super::*;

/// Verifies that live provider information refresh is an explicit terminal
/// command and that the result is cached for later model-list displays.
///
/// Ordinary pane interaction should not fetch provider catalogs on demand; this
/// command is the user-visible refresh path after daemon startup has completed.
#[tokio::test]
async fn runtime_terminal_refresh_provider_info_populates_model_catalog_cache() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(100, 40).unwrap(), 120)
        .unwrap();

    let output = service
        .execute_terminal_command_async(&primary, "refresh-provider-info")
        .await
        .unwrap();

    assert!(
        output.contains(r#""command":"refresh-provider-info""#),
        "{output}"
    );
    assert!(
        output.contains("providers=1 refreshed=1 failed=0"),
        "{output}"
    );
    assert!(output.contains("openai source=config"), "{output}");
    assert!(output.contains("provider_error=none"), "{output}");
    assert!(service.provider_model_catalog_cache.contains_key("openai"));
}

/// Verifies that `/model list` uses the active provider catalog surface instead
/// of listing only manually named profiles. In this test there is no auth store
/// attached, so the runtime must fall back to the configured provider model set
/// and clearly label the catalog source while still exposing reasoning choices.
#[tokio::test]
async fn runtime_agent_shell_model_list_displays_provider_model_catalog() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(model_list.contains(r#""command":"model""#), "{model_list}");
    assert!(
        model_list.contains(r#""content_type":"text/markdown; charset=utf-8""#),
        "{model_list}"
    );
    assert!(model_list.contains("## Model Catalog"), "{model_list}");
    assert!(!model_list.contains("### Active Selection"), "{model_list}");
    assert!(!model_list.contains("### Available Models"), "{model_list}");
    assert!(
        !model_list.contains("Provider catalog unavailable"),
        "{model_list}"
    );
    assert!(
        model_list.contains(
            "| Provider | Model | Reasoning levels | Context limit | Source | Active profile |"
        ),
        "{model_list}"
    );
    assert!(
        model_list.contains("| openai | ★ gpt-5.5 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| openai | gpt-5.4 |"), "{model_list}");
    assert!(
        model_list.contains("★ default, low, medium, high, xhigh"),
        "{model_list}"
    );
    assert!(!model_list.contains("### Quota Usage"), "{model_list}");
    assert!(!model_list.contains("provider quota"), "{model_list}");
    assert!(!model_list.contains("**Usage:**"), "{model_list}");
}

/// Verifies that an explicitly empty provider model list still falls back to
/// the provider's built-in code-defined catalog. This protects minimal configs
/// that clear `providers.openai.models` from losing all local model selection
/// when live provider catalog access is unavailable.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_code_defaults_when_config_models_empty() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = []\ndefault_model = \"gpt-5.6-sol\"\n"
                .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    for model in [
        "★ gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
    ] {
        assert!(model_list.contains(model), "{model_list}");
    }
    assert!(!model_list.contains("codex-mini-latest"), "{model_list}");
    assert!(model_list.contains("| config |"), "{model_list}");
}

/// Verifies that live provider model catalogs take precedence over configured
/// fallback models. The configured `providers.openai.models` list should keep
/// the command useful when the provider cannot be reached, but it must not
/// override a successfully populated provider catalog.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_provider_catalog_over_configured_models() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"configured-only\"]\ndefault_model = \"configured-only\"\n"
                .to_string(),
        }])
        .unwrap();
    service.cache_provider_model_catalog_for_tests(
        "openai",
        vec![crate::agent::ProviderModelInfo {
            id: "provider-only".to_string(),
            display_name: None,
            reasoning_levels: vec!["low".to_string(), "high".to_string()],
            context_window_tokens: None,
            capabilities: Vec::new(),
        }],
        vec!["low".to_string(), "high".to_string()],
    );
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(
        model_list.contains("| openai | provider-only |"),
        "{model_list}"
    );
    assert!(!model_list.contains("configured-only"), "{model_list}");
    assert!(model_list.contains("| provider |"), "{model_list}");
}

/// Verifies that Claude Code providers skip live model-catalog lookups and
/// expose the configured model list directly. This protects the documented
/// configured-model fallback path without surfacing provider-catalog errors for
/// the subprocess adapter.
#[tokio::test]
async fn runtime_agent_shell_model_list_uses_claude_code_configured_models() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: r#"[agents]
default_provider = "claude-code"
default_model_profile = "default"

[providers.claude-code]
kind = "claude-code"
api = "claude-code"
models = ["claude-sonnet-4", "claude-opus-4"]
default_model = "claude-sonnet-4"
"#
            .to_string(),
        }])
        .unwrap();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(
        !model_list.contains("Provider catalog unavailable"),
        "{model_list}"
    );
    assert!(
        model_list.contains("| claude-code | ★ claude-sonnet-4 |"),
        "{model_list}"
    );
    assert!(
        model_list.contains("| claude-code | claude-opus-4 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| config |"), "{model_list}");
}

/// Verifies that ChatGPT browser/device credentials do not trigger a fabricated
/// Codex model-catalog HTTP request. The runtime should skip that unsupported
/// live catalog path and fall back to configured provider models without
/// surfacing an OpenAI 400-class provider error in the agent prompt.
#[tokio::test]
async fn runtime_agent_shell_model_list_skips_browser_auth_catalog_request() {
    let mut service = test_runtime_service();
    service
        .replace_config_layers(vec![ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[agents]\ndefault_provider = \"openai\"\ndefault_model_profile = \"default\"\n\n[providers.openai]\nkind = \"openai\"\nmodels = [\"gpt-5.5\", \"gpt-5.4\"]\ndefault_model = \"gpt-5.5\"\n"
                .to_string(),
        }])
        .unwrap();
    let root = temp_root("runtime-model-list-chatgpt");
    let auth_store = AuthStore::new(crate::auth::AuthPaths::under_config_root(&root));
    let credential_store = auth_store.file_credential_store("openai").unwrap();
    auth_store
        .login_openai_provider_credential(
            "default",
            crate::auth::OpenAiProviderCredential {
                api_key: "chatgpt-access-token".to_string(),
                refresh_token: Some("refresh-token".to_string()),
                account_id: Some("acct_123".to_string()),
                organization_id: None,
                token_expires_at: Some("12345".to_string()),
            },
            &credential_store,
        )
        .unwrap();
    service.set_auth_store(auth_store);
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
        .unwrap();
    service
        .agent_shell_store_mut()
        .enter_or_resume("%1")
        .unwrap();

    let model_list = service
        .execute_agent_shell_command_async(&primary, "/model list")
        .await
        .unwrap();

    assert!(model_list.contains(r#""kind":"display""#), "{model_list}");
    assert!(
        !model_list.contains("Provider catalog unavailable"),
        "{model_list}"
    );
    assert!(!model_list.contains("status 400"), "{model_list}");
    assert!(!model_list.contains("Models API returned"), "{model_list}");
    assert!(
        model_list.contains("| openai | ★ gpt-5.5 |"),
        "{model_list}"
    );
    assert!(model_list.contains("| openai | gpt-5.4 |"), "{model_list}");
}
