//! CLI regressions for local durable lease administration.

use super::*;

/// The typed lease hierarchy exposes every documented local operation.
#[test]
fn lease_help_lists_complete_administration_surface() {
    let (env, home) = test_env("lease-help");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with_plain(
        vec!["mez".to_string(), "lease".to_string(), "--help".to_string()],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    let output = String::from_utf8(stdout).unwrap();
    for command in [
        "list",
        "show",
        "checkpoint",
        "recover",
        "release",
        "revoke",
        "gc",
    ] {
        assert!(output.contains(command), "{output}");
    }
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// CLI flags are serialized to the protected host RPC without credentials or
/// an intermediate direct-session initialization.
#[test]
fn lease_revoke_shapes_local_host_request() {
    let (env, home) = test_env("lease-revoke-request");
    let runtime_root = default_socket_directory(&env.runtime).unwrap().path;
    crate::runtime::ensure_private_socket_directory(&runtime_root, env.runtime.uid).unwrap();
    let socket = crate::host::server::host_socket_path(&runtime_root).unwrap();
    let listener = bind_control_socket(&socket, env.runtime.uid).unwrap();
    let server = thread::spawn(move || {
        let (_probe, _) = listener.accept().unwrap();
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (body, _) = decode_control_frame(&request, 4096).unwrap();
        let request: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(request["method"], "lease/revoke");
        assert_eq!(request["params"]["target"], "lease-1");
        assert_eq!(request["params"]["reason"], "maintenance");
        assert_eq!(request["params"]["terminate"], true);
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"host-cli","result":{"lease_id":"lease-1","state":"revoked"}}"#,
            ))
            .unwrap();
    });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    run_with(
        vec![
            "mez".to_string(),
            "lease".to_string(),
            "revoke".to_string(),
            "lease-1".to_string(),
            "--reason".to_string(),
            "maintenance".to_string(),
            "--terminate".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();
    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""state":"revoked""#), "{output}");
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Remote transport selection cannot broaden attach/create authority into
/// lease administration.
#[test]
fn lease_administration_rejects_remote_profiles_before_network_access() {
    let (env, home) = test_env("lease-remote-denied");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let error = run_with(
        vec![
            "mez".to_string(),
            "--iroh-profile".to_string(),
            "host-profile".to_string(),
            "lease".to_string(),
            "list".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap_err();
    assert_eq!(error.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(error.message().contains("local host socket"));
    assert!(stdout.is_empty());
    let _ = fs::remove_dir_all(home);
}
