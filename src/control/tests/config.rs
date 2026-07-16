//! Control config tests.

use super::*;

/// Exercises the authorized configuration control path for effective reads,
/// validation, and reload. The protocol defines `effective` as a Boolean, so
/// both Boolean values must be accepted while the implementation currently
/// returns the effective configuration envelope for either value.
#[test]
fn config_control_get_reload_and_validation_use_authorized_dispatch() {
    let (mut session, primary) = test_session();
    let layers = vec![
        ConfigLayer {
            name: "defaults".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 10000\n".to_string(),
        },
        ConfigLayer {
            name: "primary".to_string(),
            path: Some(PathBuf::from("/home/user/.config/mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 2000\npersist = true\n[mcp_servers.fs]\nargs = [\"--root\", \".\"]\n"
                .to_string(),
        },
    ];
    let mut idempotency = ControlIdempotencyCache::default();

    let get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","effective":true}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    let get_json: serde_json::Value = serde_json::from_str(&get).unwrap();
    assert_eq!(get_json["result"]["value"], 2000);
    assert!(get.contains(r#""source":"primary""#));
    assert!(get.contains(r#""layers""#));
    assert!(get.contains(r#""id":"defaults""#));
    assert!(get.contains(r#""layer_type":"user""#));
    assert!(get.contains(r#""precedence":0"#));
    assert!(get.contains(r#""trusted":true"#));
    assert!(get.contains(r#""applied":true"#));
    assert!(get.contains(r#""schema_version":1"#));
    assert!(get.contains(r#""diagnostics":[]"#));

    let full_get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":20,"method":"config/get","params":{"effective":true}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    let full_get_json: serde_json::Value = serde_json::from_str(&full_get).unwrap();
    assert_eq!(full_get_json["result"]["value"]["history.lines"], 2000);
    assert_eq!(full_get_json["result"]["value"]["history.persist"], true);
    assert_eq!(
        full_get_json["result"]["value"]["mcp_servers.fs.args"],
        serde_json::json!(["--root", "."])
    );

    let explicit_non_effective_get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":21,"method":"config/get","params":{"path":"history.lines","effective":false}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    let explicit_non_effective_json: serde_json::Value =
        serde_json::from_str(&explicit_non_effective_get).unwrap();
    assert_eq!(explicit_non_effective_json["result"]["value"], 2000);
    assert_eq!(explicit_non_effective_json["result"]["source"], "primary");

    let validate = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":2,"method":"config/validate","params":{}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    assert!(validate.contains(r#""valid":true"#));
    assert_eq!(session.config_generation, 0);

    let reload = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":3,"method":"config/reload","params":{"idempotency_key":"reload-config"}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    assert!(reload.contains(r#""operation":"reload""#));
    assert!(reload.contains(r#""layers""#));
    assert!(reload.contains(r#""applied":true"#));
    assert!(reload.contains(r#""status":"applied""#));
    assert_eq!(session.config_generation, 1);

    let cached_reload = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":4,"method":"config/reload","params":{"idempotency_key":"reload-config"}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );
    assert_eq!(cached_reload, reload);
    assert_eq!(session.config_generation, 1);
}

/// Verifies that the config control surface rejects unknown request fields
/// before handler-specific parsing. Config methods support a specialized
/// dispatcher for layer state and idempotency, so unknown fields must be checked
/// there as well as in the generic control path.
#[test]
fn config_control_methods_reject_unknown_params_on_specialized_dispatch() {
    let (mut session, primary) = test_session();
    let mut idempotency = ControlIdempotencyCache::default();

    let get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","surprise":true}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(get.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(get.contains("config/get params contains unknown field"));

    let invalid_effective = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":5,"method":"config/get","params":{"path":"history.lines","effective":"false"}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(invalid_effective.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(invalid_effective.contains("config/get effective must be a boolean"));

    let validate = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":3,"method":"config/validate","params":{"scope":"project"}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(validate.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(validate.contains("config/validate params contains unknown field"));

    let set = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":2,"method":"config/set","params":{"path":"history.lines","value":2048,"idempotency_key":"set","surprise":true}}"#,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert!(set.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(set.contains("config/set params contains unknown field"));
}

/// Verifies config control get reports per layer diagnostics.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_get_reports_per_layer_diagnostics() {
    let (mut session, primary) = test_session();
    let layers = vec![
        ConfigLayer {
            name: "primary".to_string(),
            path: None,
            format: ConfigFormat::Toml,
            scope: ConfigScope::Primary,
            trusted: true,
            text: "[history]\nlines = 2000\n".to_string(),
        },
        ConfigLayer {
            name: "project".to_string(),
            path: Some(PathBuf::from("/workspace/.mezzanine/config.toml")),
            format: ConfigFormat::Toml,
            scope: ConfigScope::ProjectOverlay,
            trusted: false,
            text: "[history]\nlines = 7\n".to_string(),
        },
    ];
    let mut idempotency = ControlIdempotencyCache::default();

    let get = dispatch_control_request_for_client_with_config(
        r#"{"jsonrpc":"2.0","id":1,"method":"config/get","params":{"path":"history.lines","effective":true}}"#,
        &mut session,
        &primary,
        &layers,
        &mut idempotency,
    );

    let get_json: serde_json::Value = serde_json::from_str(&get).unwrap();
    assert_eq!(get_json["result"]["value"], 2000);
    assert!(get.contains(r#""id":"project""#), "{get}");
    assert!(get.contains(r#""layer_type":"project_root""#), "{get}");
    assert!(get.contains(r#""applied":false"#), "{get}");
    assert!(get.contains(r#""state":"skipped""#), "{get}");
    assert!(
        get.contains("project overlay is pending trust and was not applied"),
        "{get}"
    );
}

/// Verifies config control mutations persist explicit targets and are cached.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_mutations_persist_explicit_targets_and_are_cached() {
    let (mut session, primary) = test_session();
    let root = temp_root("config-mutation");
    let primary_path = root.join("config.toml");
    let project_path = root.join(".mezzanine").join("config.toml");
    fs::write(&primary_path, "[history]\nlines = 10000\npersist = true\n").unwrap();
    fs::create_dir_all(project_path.parent().unwrap()).unwrap();
    fs::write(&project_path, "[history]\nlines = 50\npersist = true\n").unwrap();
    let mut idempotency = ControlIdempotencyCache::default();
    let primary_path_json = json_escape(&primary_path.to_string_lossy());
    let project_path_json = json_escape(&project_path.to_string_lossy());
    let set_request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"config/set","params":{{"path":"history.lines","value":2048,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"set-lines"}}}}"#,
        primary_path_json
    );

    let first = dispatch_control_request_for_client_with_config(
        &set_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    let second = dispatch_control_request_for_client_with_config(
        &set_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );

    assert_eq!(first, second);
    assert_eq!(idempotency.len(), 1);
    assert_eq!(session.config_generation, 1);
    assert!(first.contains(r#""applied":true"#));
    assert!(first.contains(r#""persisted":true"#));
    assert!(first.contains(r#""scope":"user""#));
    assert!(
        fs::read_to_string(&primary_path)
            .unwrap()
            .contains("lines = 2048")
    );

    let set_array_request = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"config/set","params":{{"path":"mcp_servers.fs.args","value":["--root","."],"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"set-array"}}}}"#,
        primary_path_json
    );
    let array_response = dispatch_control_request_for_client_with_config(
        &set_array_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(array_response.contains(r#""applied":true"#));
    assert_eq!(session.config_generation, 2);
    assert!(
        fs::read_to_string(&primary_path)
            .unwrap()
            .contains(r#"args = ["--root", "."]"#)
    );

    let conflict_request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"config/set","params":{{"path":"history.lines","value":4096,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"set-lines"}}}}"#,
        primary_path_json
    );
    let conflict = dispatch_control_request_for_client_with_config(
        &conflict_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(conflict.contains(r#""mezzanine_code":"conflict""#));
    assert_eq!(session.config_generation, 2);
    assert!(
        fs::read_to_string(&primary_path)
            .unwrap()
            .contains("lines = 2048")
    );

    let unset_project = format!(
        r#"{{"jsonrpc":"2.0","id":3,"method":"config/unset","params":{{"path":"history.persist","persist":{{"scope":"project","path":"{}"}},"idempotency_key":"unset-project"}}}}"#,
        project_path_json
    );
    let project_response = dispatch_control_request_for_client_with_config(
        &unset_project,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(project_response.contains(r#""scope":"project""#));
    assert_eq!(session.config_generation, 3);
    assert!(
        !fs::read_to_string(&project_path)
            .unwrap()
            .contains("persist")
    );

    let primary_scope_request = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"config/set","params":{{"path":"history.lines","value":8192,"persist":{{"scope":"primary","path":"{}"}},"idempotency_key":"set-primary-scope"}}}}"#,
        primary_path_json
    );
    let primary_scope_response = dispatch_control_request_for_client_with_config(
        &primary_scope_request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
    );
    assert!(primary_scope_response.contains(r#""mezzanine_code":"invalid_params""#));
    assert!(primary_scope_response.contains("must be live, user, or project"));
    assert_eq!(session.config_generation, 3);

    let _ = fs::remove_dir_all(root);
}

/// Verifies config control mutations can emit required audit records.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_mutations_can_emit_required_audit_records() {
    let (mut session, primary) = test_session();
    let root = temp_root("config-audit");
    let config_path = root.join("config.toml");
    fs::write(&config_path, "[history]\nlines = 10000\n").unwrap();
    let audit_path = root.join("audit.jsonl");
    let mut audit_log = AuditLog::new(crate::audit::AuditConfig {
        enabled: true,
        path: audit_path.clone(),
        hash_chain: false,
        required: true,
    });
    let mut idempotency = ControlIdempotencyCache::default();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"config/set","params":{{"path":"history.lines","value":2048,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"config-audit"}}}}"#,
        json_escape(&config_path.to_string_lossy())
    );

    let response = dispatch_control_request_for_client_with_config_and_audit(
        &request,
        &mut session,
        &primary,
        &[],
        &mut idempotency,
        &mut audit_log,
    );

    assert!(response.contains(r#""applied":true"#));
    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains(r#""event_type":"configuration""#));
    assert!(audit.contains(r#""action":"set""#));
    assert!(audit.contains(r#""outcome":"started""#));
    assert!(audit.contains(r#""outcome":"applied""#));
    assert!(audit.contains(r#""key":"history.lines""#));
    let _ = fs::remove_dir_all(root);
}

/// Verifies config control mutations are primary only.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn config_control_mutations_are_primary_only() {
    let (mut session, _primary) = test_session();
    let (observer_client, _observer_request) = session.request_observer("observer");
    let root = temp_root("config-observer");
    let path = root.join("config.toml");
    fs::write(&path, "[history]\nlines = 10000\n").unwrap();
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"config/set","params":{{"path":"history.lines","value":1,"persist":{{"scope":"user","path":"{}"}},"idempotency_key":"observer-set"}}}}"#,
        json_escape(&path.to_string_lossy())
    );
    let mut idempotency = ControlIdempotencyCache::default();

    let response = dispatch_control_request_for_client_with_config(
        &request,
        &mut session,
        &observer_client,
        &[],
        &mut idempotency,
    );

    assert!(response.contains(r#""mezzanine_code":"forbidden""#));
    assert!(fs::read_to_string(&path).unwrap().contains("lines = 10000"));
    assert!(idempotency.is_empty());

    let _ = fs::remove_dir_all(root);
}
