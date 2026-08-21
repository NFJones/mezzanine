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
    for command in ["status", "invite", "clients", "rename", "revoke"] {
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
