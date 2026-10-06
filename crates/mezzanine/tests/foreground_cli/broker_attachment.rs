//! Real CLI/PTY reuse of a synthetic protected outbound broker.
//!
//! The wire peer verifies credential-free fresh routing and independently live
//! frontends. Real host authorization is qualified by the product's loopback
//! tests, not this peer. Disposable profiles never use developer credentials.

use super::*;
use serde_json::{Value, json};

/// Reads one exact bounded local protocol frame without losing pipelined bytes.
fn read_frame(stream: &mut std::os::unix::net::UnixStream) -> std::io::Result<Value> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 8192 {
            return Err(std::io::Error::other("fixture header exceeds limit"));
        }
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        header.push(byte[0]);
    }
    let text = std::str::from_utf8(&header).map_err(std::io::Error::other)?;
    let length = text
        .split("\r\n")
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .filter(|length| *length <= 1024 * 1024)
        .ok_or_else(|| std::io::Error::other("fixture length invalid"))?;
    let mut body = vec![0; length];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}

/// Sends only the closed outbound MIME, not a raw remote initialization reply.
fn send_frame(stream: &mut std::os::unix::net::UnixStream, body: Value) -> std::io::Result<()> {
    let body = body.to_string();
    write!(
        stream,
        "Content-Type: application/vnd.mezzanine.outbound+json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )?;
    stream.flush()
}

/// Owns a finite synthetic frontend pipeline; timeout bounds failure cleanup.
fn serve_frontend(
    mut stream: std::os::unix::net::UnixStream,
    index: usize,
    settled: mpsc::Sender<Value>,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    assert_eq!(
        read_frame(&mut stream).unwrap(),
        json!({"protocol":"mez-outbound/1"})
    );
    let handle = json!({"owner":"00000000000000000000000000000001","generation":index + 1});
    send_frame(
        &mut stream,
        json!({"protocol":"mez-outbound/1","handle":handle}),
    )
    .unwrap();
    let setup = read_frame(&mut stream).unwrap();
    assert!(setup["initialize"].get("authentication").is_none());
    assert_eq!(setup["initialize"]["session_intent"], "create");
    assert_eq!(setup["initialize"]["event_stream_version"], 2);
    let view = read_frame(&mut stream).unwrap();
    assert_eq!(view["handle"], handle);
    let summary = json!({"selected_version":3,"granted_role":"primary",
        "session_id":format!("${}", index + 1),"lease_id":format!("lease-{}", index + 1),
        "client_id":format!("c{}", index + 1)});
    send_frame(
        &mut stream,
        json!({"handle":handle,"session":summary,
        "lines":[format!("BROKER-FRONTEND-{}", index + 1)],"line_style_spans":[[]],
        "cursor":{"row":0,"column":0,"visible":false},"output_modes":{},"presentation_ids":[]}),
    )
    .unwrap();
    settled.send(setup).unwrap();
    while let Ok(request) = read_frame(&mut stream) {
        assert_eq!(
            request["operation"], "items",
            "idle client must not invent mutations"
        );
        thread::sleep(Duration::from_millis(25));
        if send_frame(
            &mut stream,
            json!({"kind":"redraw","handle":handle,"session":summary,
            "action":"none","event_id":null}),
        )
        .is_err()
        {
            break;
        }
    }
}

/// Launches the real new-session command on an independently owned PTY.
fn spawn_client(root: &Path, home: &Path, runtime: &Path, name: &str) -> ForegroundProcess {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_mez"));
    command.env_clear();
    command.env("HOME", home.as_os_str());
    command.env("SHELL", "/bin/sh");
    command.env("MEZ_TMPDIR", runtime.as_os_str());
    command.env("TERM", "xterm-256color");
    command.args(["--iroh-profile", "fixture", "new", "--name", name]);
    let child = pair.slave.spawn_command(command).unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let writer = pair.master.take_writer().unwrap();
    drop(pair.slave);
    let (output_tx, output_rx) = mpsc::channel();
    let reader_thread = thread::spawn(move || {
        let mut bytes = [0; 4096];
        while let Ok(read) = reader.read(&mut bytes) {
            if read == 0 || output_tx.send(bytes[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    ForegroundProcess {
        child,
        master: pair.master,
        writer: Some(writer),
        output_rx,
        reader_thread: Some(reader_thread),
        root: root.join(name),
    }
}

/// Two ordinary new commands share one protected broker while the first remains
/// live. Fresh invocation keys and names remain distinct; SIGINT retires only
/// each frontend and restores presentation. No competing endpoint key is created.
#[test]
fn foreground_new_reuses_live_broker_for_independent_frontends() {
    let root = test_root("broker-new");
    let home = root.join("home");
    let config = home.join(".config/mezzanine");
    let client = config.join("remote/client");
    let credentials = client.join("credentials");
    let runtime = root.join("runtime");
    fs::create_dir_all(&credentials).unwrap();
    fs::create_dir_all(&runtime).unwrap();
    for directory in [
        &home.join(".config"),
        &config,
        &config.join("remote"),
        &client,
        &credentials,
        &runtime,
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = json!({"version":1,"profiles":[{"name":"fixture",
        "server_addr":iroh::EndpointAddr::new(iroh::SecretKey::generate().public()),
        "role":"primary","scope":"host","credential_file":"fixture.secret"}]});
    fs::write(client.join("profiles.json"), profile.to_string()).unwrap();
    fs::write(
        credentials.join("fixture.secret"),
        "synthetic-private-proof",
    )
    .unwrap();
    for file in [
        client.join("profiles.json"),
        credentials.join("fixture.secret"),
    ] {
        fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let socket = config.join("outbound.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (settled_tx, settled_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut workers = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while workers.len() < 2 && Instant::now() < deadline {
            match listener.accept() {
                Ok((stream, _)) => {
                    let sender = settled_tx.clone();
                    let index = workers.len();
                    workers.push(thread::spawn(move || serve_frontend(stream, index, sender)));
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let mut first = spawn_client(&root, &home, &runtime, "first");
    let first_setup = settled_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("ordinary new must reuse broker");
    let mut first_output = Vec::new();
    first
        .read_until(&mut first_output, Duration::from_secs(5), |text| {
            text.contains("BROKER-FRONTEND-1")
        })
        .unwrap();
    let mut second = spawn_client(&root, &home, &runtime, "second");
    let second_setup = settled_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("second new must reuse live broker");
    let mut second_output = Vec::new();
    second
        .read_until(&mut second_output, Duration::from_secs(5), |text| {
            text.contains("BROKER-FRONTEND-2")
        })
        .unwrap();
    assert_ne!(
        first_setup["initialize"]["idempotency_key"],
        second_setup["initialize"]["idempotency_key"]
    );
    assert_eq!(
        first_setup["initialize"]["client"]["metadata"]["session_name"],
        "first"
    );
    assert_eq!(
        second_setup["initialize"]["client"]["metadata"]["session_name"],
        "second"
    );
    assert!(first.child.try_wait().unwrap().is_none());
    first.send_interrupt().unwrap();
    first
        .read_until_exit(&mut first_output, Duration::from_secs(5))
        .unwrap();
    assert!(second.child.try_wait().unwrap().is_none());
    second.send_interrupt().unwrap();
    second
        .read_until_exit(&mut second_output, Duration::from_secs(5))
        .unwrap();
    assert!(!client.join("endpoint.key").exists());
    assert!(String::from_utf8_lossy(&first_output).contains("\x1b[?1049l"));
    assert!(String::from_utf8_lossy(&second_output).contains("\x1b[?1049l"));
    drop(first);
    drop(second);
    server.join().unwrap();
    fs::remove_dir_all(root).unwrap();
}
