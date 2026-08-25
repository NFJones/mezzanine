//! CLI regressions for local remote-transport administration.

use super::*;

/// Binds an explicit control socket or skips on restricted test hosts.
fn remote_control_listener(
    env: &CliEnv,
    home: &Path,
) -> Option<(PathBuf, std::os::unix::net::UnixListener)> {
    let socket = home.join("runtime").join("remote.sock");
    match bind_control_socket(&socket, env.runtime.uid) {
        Ok(listener) => Some((socket, listener)),
        Err(error)
            if error.kind() == crate::error::MezErrorKind::Io
                && error.message().contains("Operation not permitted") =>
        {
            None
        }
        Err(error) => panic!("{error}"),
    }
}

/// Verifies the remote hierarchy exposes every local administration command.
#[test]
fn remote_help_lists_local_administration_commands() {
    let (env, home) = test_env("remote-help");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "remote".to_string(),
            "--help".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    for command in [
        "status",
        "invite",
        "pair",
        "invitation",
        "profile",
        "clients",
        "rename",
        "revoke",
    ] {
        assert!(output.contains(command), "{output}");
    }
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies status uses the selected Unix control socket and preserves JSON.
#[test]
fn remote_status_uses_selected_control_socket_with_json_output() {
    let (env, home) = test_env("remote-status-control");
    let Some((socket, listener)) = remote_control_listener(&env, &home) else {
        let _ = fs::remove_dir_all(home);
        return;
    };
    let server = spawn_noninteractive_attach_stub_server(
        listener,
        Some(r#""method":"remote/status""#),
        r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
        Some(
            r#"{"jsonrpc":"2.0","id":"cli","result":{"enabled":false,"endpoint_id":"server-endpoint","active_remote_connections":0}}"#,
        ),
    );
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().into_owned(),
            "remote".to_string(),
            "status".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(
        output.contains(r#""endpoint_id":"server-endpoint""#),
        "{output}"
    );
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies invitation role, expiry, idempotency, and plain secret output.
#[test]
fn remote_invite_sends_bounded_params_and_renders_plain_output() {
    let (env, home) = test_env("remote-invite-control");
    let Some((socket, listener)) = remote_control_listener(&env, &home) else {
        let _ = fs::remove_dir_all(home);
        return;
    };
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (initialize, _) = decode_control_frame(&request, 4096).unwrap();
        assert!(initialize.contains(r#""method":"control/initialize""#));
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
            ))
            .unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (invite, _) = decode_control_frame(&request, 4096).unwrap();
        assert!(invite.contains(r#""method":"remote/invite""#), "{invite}");
        assert!(invite.contains(r#""role":"primary""#), "{invite}");
        assert!(invite.contains(r#""expires_seconds":120"#), "{invite}");
        assert!(invite.contains(r#""allow_create":true"#), "{invite}");
        assert!(invite.contains(r#""allow_kill":true"#), "{invite}");
        assert!(invite.contains(r#""max_leases":3"#), "{invite}");
        assert!(invite.contains(r#""max_live_sessions":2"#), "{invite}");
        assert!(
            invite.contains(r#""lease_lifetime_ceiling_seconds":3600"#),
            "{invite}"
        );
        assert!(invite.contains(r#""idempotency_key":""#), "{invite}");
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli","result":{"invitation_id":"invite-1","token":"pairing-secret","role":"primary","expires_at_unix_seconds":999}}"#,
            ))
            .unwrap();
    });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().into_owned(),
            "remote".to_string(),
            "invite".to_string(),
            "--role".to_string(),
            "primary".to_string(),
            "--allow-create".to_string(),
            "--allow-kill".to_string(),
            "--max-leases".to_string(),
            "3".to_string(),
            "--max-live-sessions".to_string(),
            "2".to_string(),
            "--lease-lifetime-ceiling".to_string(),
            "3600".to_string(),
            "--expires".to_string(),
            "120".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("pairing-secret"), "{output}");
    assert!(output.contains("primary"), "{output}");
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies an omitted CLI expiry remains absent so the server can apply its
/// configured invitation lifetime instead of receiving a hard-coded default.
#[test]
fn remote_invite_omits_unspecified_expiry() {
    let (env, home) = test_env("remote-invite-configured-expiry");
    let Some((socket, listener)) = remote_control_listener(&env, &home) else {
        let _ = fs::remove_dir_all(home);
        return;
    };
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (initialize, _) = decode_control_frame(&request, 4096).unwrap();
        assert!(initialize.contains(r#""method":"control/initialize""#));
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
            ))
            .unwrap();
        let request = read_control_response_frames(&mut stream, 4096, 1).unwrap();
        let (invite, _) = decode_control_frame(&request, 4096).unwrap();
        assert!(invite.contains(r#""method":"remote/invite""#), "{invite}");
        assert!(invite.contains(r#""role":"observer""#), "{invite}");
        assert!(!invite.contains("expires_seconds"), "{invite}");
        assert!(invite.contains(r#""idempotency_key":""#), "{invite}");
        stream
            .write_all(&encode_control_body(
                r#"{"jsonrpc":"2.0","id":"cli","result":{"invitation_id":"invite-1","token":"pairing-secret","role":"observer","expires_at_unix_seconds":999}}"#,
            ))
            .unwrap();
    });
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with_plain(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().into_owned(),
            "remote".to_string(),
            "invite".to_string(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("pairing-secret"), "{output}");
    assert!(output.contains("observer"), "{output}");
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies secure invitation output creates a private file without printing
/// its bearer token to command output.
///
/// The server response intentionally contains a secret. The CLI must preserve
/// that complete response in the requested file while returning only the path.
#[test]
fn remote_invite_output_is_private_and_does_not_echo_token() {
    let (env, home) = test_env("remote-invite-output");
    let Some((socket, listener)) = remote_control_listener(&env, &home) else {
        let _ = fs::remove_dir_all(home);
        return;
    };
    let server = spawn_noninteractive_attach_stub_server(
        listener,
        Some(r#""method":"remote/invite""#),
        r#"{"jsonrpc":"2.0","id":"cli-init","result":{"granted_role":"primary"}}"#,
        Some(
            r#"{"jsonrpc":"2.0","id":"cli","result":{"format_version":1,"invitation_id":"invite-1","token":"pairing-secret","role":"primary","expires_at_unix_seconds":999}}"#,
        ),
    );
    let output_path = home.join("mez-invite.json");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "-S".to_string(),
            socket.to_string_lossy().into_owned(),
            "remote".to_string(),
            "invite".to_string(),
            "--role".to_string(),
            "primary".to_string(),
            "--output".to_string(),
            output_path.to_string_lossy().into_owned(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();
    server.join().unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains("mez-invite.json"), "{output}");
    assert!(!output.contains("pairing-secret"), "{output}");
    assert!(
        fs::read_to_string(&output_path)
            .unwrap()
            .contains("pairing-secret")
    );
    assert_eq!(
        fs::metadata(&output_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies invitation inspection exposes only versioned, redacted metadata.
///
/// The invitation token and full endpoint identity are bearer-sensitive or
/// unnecessarily identifying and must not appear in either output mode.
#[test]
fn remote_invitation_inspect_redacts_token_and_full_endpoint_identity() {
    let (env, home) = test_env("remote-invitation-inspect");
    let endpoint = iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
        .with_ip_addr("192.0.2.10:4242".parse().unwrap())
        .with_relay_url("https://relay.example".parse().unwrap());
    let endpoint_id = endpoint.id.to_string();
    let invitation_path = home.join("inspect-invite.json");
    let invitation = serde_json::json!({
        "format_version": 1,
        "server_endpoint_id": endpoint_id,
        "server_addr": endpoint,
        "profile_name": "server-session",
        "token": "pairing-secret",
        "role": "primary",
        "expires_at_unix_seconds": u64::MAX,
    });
    fs::write(
        &invitation_path,
        serde_json::to_vec_pretty(&invitation).unwrap(),
    )
    .unwrap();
    fs::set_permissions(&invitation_path, fs::Permissions::from_mode(0o600)).unwrap();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_with(
        vec![
            "mez".to_string(),
            "remote".to_string(),
            "invitation".to_string(),
            "inspect".to_string(),
            invitation_path.to_string_lossy().into_owned(),
        ],
        env,
        false,
        &mut stdout,
        &mut stderr,
    )
    .unwrap();

    let output = String::from_utf8(stdout).unwrap();
    assert!(output.contains(r#""format_version":1"#), "{output}");
    assert!(output.contains(r#""direct_route_count":1"#), "{output}");
    assert!(output.contains(r#""relay_route_count":1"#), "{output}");
    assert!(!output.contains("pairing-secret"), "{output}");
    assert!(!output.contains(&endpoint_id), "{output}");
    assert!(stderr.is_empty());
    let _ = fs::remove_dir_all(home);
}

/// Verifies local profile commands expose redacted metadata and mutate only
/// the client-local alias and selected credential record.
///
/// The device credential must never be rendered, and removing a profile must
/// state that server trust was not revoked.
#[test]
fn remote_profile_commands_are_redacted_and_local_only() {
    let (env, home) = test_env("remote-profile-commands");
    let paths = env.config_paths().unwrap();
    let store = crate::security::remote::RemoteClientProfileStore::under_config_root(paths.root());
    store
        .save(&crate::security::remote::RemoteClientProfile {
            name: "home".to_string(),
            server_addr: iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
                .with_ip_addr("192.0.2.20:4242".parse().unwrap()),
            role: crate::security::remote::RemoteRoleCeiling::Primary,
            scope: crate::security::remote::RemoteClientProfileScope::LegacySession,
            device_credential: secrecy::SecretString::from("device-secret".to_string()),
        })
        .unwrap();

    let run_profile = |arguments: &[&str]| {
        let mut argv = vec![
            "mez".to_string(),
            "remote".to_string(),
            "profile".to_string(),
        ];
        argv.extend(arguments.iter().map(|argument| (*argument).to_string()));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        run_with(argv, env.clone(), false, &mut stdout, &mut stderr).unwrap();
        assert!(stderr.is_empty());
        String::from_utf8(stdout).unwrap()
    };

    let listed = run_profile(&["list"]);
    assert!(listed.contains(r#""name":"home""#), "{listed}");
    assert!(!listed.contains("device-secret"), "{listed}");
    let renamed = run_profile(&["rename", "home", "home-mez"]);
    assert!(renamed.contains(r#""name":"home-mez""#), "{renamed}");
    let removed = run_profile(&["remove", "home-mez"]);
    assert!(
        removed.contains(r#""server_trust_revoked":false"#),
        "{removed}"
    );
    assert!(store.load("home-mez").unwrap().is_none());

    let _ = fs::remove_dir_all(home);
}
