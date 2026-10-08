//! Opt-in real curated-command creator evidence without a provider conversation.
//!
//! An owned temporary mod uses literal classic.SessionStart/next/process.run
//! calls. --init-only runs Setup/SessionStart then exits. HOME/config/cwd/env are
//! isolated and no credentials/prompt, installation, policy bypass, supplied PID
//! or enrollment capability exists. This qualifies only a source relationship,
//! not a deployed idle producer, enabled integration or authorization contract.
//! A separate ordinary Node SDK shim qualifies the shared source after callback
//! return through real native ingress; it is not real vendor idle qualification.

use super::*;
use tokio::io::AsyncWriteExt;

/// Shared shell boundary used by the source probe and isolation regression.
fn start_probe_shell(service: &mut RuntimeSessionService, directory: &std::path::Path) {
    let environment = vec![
        ("PATH".into(), "/usr/bin:/bin".into()),
        (
            "HOME".into(),
            directory.join("home").to_str().unwrap().into(),
        ),
        (
            "CLAUDE_CONFIG_DIR".into(),
            directory.join("config").to_str().unwrap().into(),
        ),
        ("TERM".into(), "xterm-256color".into()),
    ];
    service
        .start_initial_pane_process_with_launch_context(
            None,
            Some(&directory.join("work")),
            Some(&environment),
        )
        .unwrap();
}

/// Separate subprocess isolation avoids mutating the parallel runner environment.
/// Hostile-but-harmless startup/shadow scripts only create owned marker files.
#[test]
fn external_claude_curated_shell_start_ignores_hostile_env_and_path() {
    use std::os::unix::fs::PermissionsExt;
    let directory = std::path::Path::new("/tmp").join(format!(
        "mez-curated-env-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _directory = Directory(directory.clone());
    for name in ["home", "config", "work", "hostile"] {
        std::fs::create_dir(directory.join(name)).unwrap();
    }
    let quote = |path: &std::path::Path| {
        shlex::try_quote(path.to_str().unwrap())
            .unwrap()
            .into_owned()
    };
    std::fs::write(
        directory.join("startup.sh"),
        format!(
            "printf invoked > {}\n",
            quote(&directory.join("startup-ran"))
        ),
    )
    .unwrap();
    std::fs::write(
        directory.join("hostile/env"),
        format!(
            "#!/bin/sh\nprintf invoked > {}\nexit 1\n",
            quote(&directory.join("shadow-ran"))
        ),
    )
    .unwrap();
    std::fs::set_permissions(
        directory.join("hostile/env"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "runtime::control::external_enrollment::tests::claude_curated::external_claude_curated_shell_isolation_fixture", "--ignored", "--quiet"])
        .env("MEZ_TEST_CURATED_ROOT", &directory).env("ENV", directory.join("startup.sh"))
        .env("PATH", directory.join("hostile")).output().unwrap();
    assert!(
        !directory.join("startup-ran").exists(),
        "inherited ENV startup code executed"
    );
    assert!(
        !directory.join("shadow-ran").exists(),
        "inherited PATH shadow command executed"
    );
    assert!(
        output.status.success(),
        "owned shell isolation fixture failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout[..output.stdout.len().min(8192)]),
        String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(8192)])
    );
    assert_eq!(std::fs::read(directory.join("clean")).unwrap(), b"clean");
}

/// Only the parent regression invokes this helper. It launches the same boundary
/// as the vendor probe and uses a harmless unqualified env command to detect PATH
/// inheritance before the later env -i command could hide it.
#[test]
#[ignore = "self-executing hostile-environment shell fixture"]
fn external_claude_curated_shell_isolation_fixture() {
    let Some(directory) = std::env::var_os("MEZ_TEST_CURATED_ROOT") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    let mut service = RuntimeServiceFixture::new().build();
    struct Service(RuntimeSessionService);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.terminate_all_pane_processes();
        }
    }
    start_probe_shell(&mut service, &directory);
    let mut service = Service(service);
    let report = shlex::try_quote(directory.join("clean").to_str().unwrap())
        .unwrap()
        .into_owned();
    let command = format!("env /bin/sh -c 'printf clean' > {report}\n");
    service
        .0
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let started = Instant::now();
    while !std::fs::read(directory.join("clean")).is_ok_and(|bytes| bytes == b"clean")
        && started.elapsed() < Duration::from_secs(5)
    {
        let _ = service.0.poll_pane_processes();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(std::fs::read(directory.join("clean")).unwrap(), b"clean");
    service.0.terminate_all_pane_processes().unwrap();
}

/// Invoked only by the actual curated process API in the parent test. The socket
/// path is nonsecret routing; kernel lifetime/writer evidence supplies identity.
#[test]
#[ignore = "self-executing curated-command native probe"]
fn external_claude_curated_command_helper_fixture() {
    let Some(path) = std::env::var_os("MEZ_TEST_CURATED_SOCKET") else {
        return;
    };
    let mut socket = std::os::unix::net::UnixStream::connect(path).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    socket.write_all(&[1]).unwrap();
    let mut release = [0];
    socket.read_exact(&mut release).unwrap();
    assert_eq!(release, [2]);
}

/// Source-only native qualification: a temporary no-Node mod actually invokes
/// the test helper through $.process.run. Its captured direct parent must be the
/// local vendor executable and a distinct pane descendant, while ancestor/source
/// lifetime is retained off actor. The executable comparison is a test assertion,
/// never executable-name authorization or a substitute for native provenance.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY; isolated --init-only source probe"]
async fn external_claude_curated_command_native_parent_is_local_pane_descendant() {
    qualify_curated_probe(false, 0, false, false, false).await;
}

/// Real curated argv API -> built fixed source helper -> current-writer Unix
/// actor -> native creator registration. Only actual inert SessionStart identity
/// is read; no Node, stdin authority, provider prompt or user plugin install.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; isolated source transport"]
async fn external_claude_curated_builtin_source_helper_registers_actual_creator() {
    qualify_curated_probe(true, 0, false, false, false).await;
}

/// The actual curated clock runs outside conversation callbacks and emits only
/// original frozen public epoch proof through the built helper. Daemon maintenance
/// may then renew a live creator's lease; neither the timer nor parent PID supplies
/// socket/control/usage authority or launches a provider conversation.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; isolated clock proof"]
async fn external_claude_curated_clock_helper_proves_original_observer_epoch() {
    qualify_curated_probe(true, 1, false, false, false).await;
}

/// Repeated actual SDK timer callbacks must advance the same original observer
/// epoch through distinct short-lived helpers, without conversation callbacks,
/// client initialization, provider work or a replacement accounting namespace.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; isolated recurring proof"]
async fn external_claude_curated_clock_every_advances_original_observer_epoch() {
    qualify_curated_probe(true, 3, false, false, false).await;
}

/// An ordinary SDK-like Node producer returns from SessionStart before timers
/// run. The same rendered source must deliver three real native helper proofs
/// under the original creator, then lose its observer without inventing process
/// death or a new namespace. This is a documented SDK shim, not real Claude idle.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and built mez; ordinary post-return proof"]
async fn external_curated_ordinary_node_clock_survives_callback_return() {
    qualify_curated_probe(true, 3, true, false, false).await;
}

/// Actual SDK argv admission supplies the captured predecessor, not an injected
/// generation. The shared successor body must keep one native creator/run,
/// reject an original-epoch proof and deliver sequences 1–3 on observer epoch2.
/// Init-only still holds SessionStart open; this is not deployed module reload.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and freshly built mez; public handoff"]
async fn external_claude_curated_fixed_helper_handoff_uses_actual_public_predecessor() {
    qualify_curated_probe(true, 3, false, true, false).await;
}

/// The actual SDK imports the pure owner, suppresses a duplicate source call,
/// and delivers only one original epoch's recurring proofs. The caller stops
/// its exact timer after the hold helper completes; no provider is started.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; owned SDK timer"]
async fn external_claude_curated_owned_source_has_one_admission_and_timer() {
    qualify_curated_probe(true, 3, false, false, true).await;
}

/// A separately owned successor keeps the actual public predecessor and native
/// run while duplicate callback work remains inert. This proves SDK import and
/// local cancellation, not cross-module persistent storage or deployed reload.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; owned SDK successor"]
async fn external_claude_curated_owned_successor_pins_one_native_epoch() {
    qualify_curated_probe(true, 3, false, true, true).await;
}

/// Separate actual classic.Setup and classic.SessionStart callbacks share the
/// exact pure owner/ticket across SDK frames. Setup reserves only inert local
/// metadata; genuine SessionStart alone admits the native producer and proofs.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; module-scoped owner"]
async fn external_claude_curated_module_owner_survives_separate_sdk_callbacks() {
    qualify_curated_probe(true, 4, false, false, true).await;
}

/// The production literal module imports both owned siblings and admits only
/// an actual main SessionStart. Its module-retained owner drives native proofs;
/// the injected test gate only keeps init-only alive, not enrollment authority.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY and built mez; literal module entry"]
async fn external_claude_curated_literal_module_owns_actual_main_session() {
    qualify_curated_probe(true, 5, false, false, true).await;
}

/// Shared owned temporary source fixture for provenance-only and real transport
/// qualification. Actor shutdown returns the owned runtime before source exit
/// assertions, preserving pane cleanup and exact native creator inspection.
async fn qualify_curated_probe(
    admission: bool,
    proofs: u64,
    node_shim: bool,
    handoff: bool,
    owned: bool,
) {
    assert!(!owned || (admission && proofs > 1 && !node_shim));
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    let vendor = std::path::PathBuf::from(
        std::env::var_os(if node_shim {
            "MEZ_TEST_NODE_BINARY"
        } else {
            "MEZ_TEST_CLAUDE_BINARY"
        })
        .expect("explicit installed offline producer executable"),
    );
    assert!(vendor.is_absolute() && vendor.is_file());
    let directory = std::path::Path::new("/tmp").join(format!(
        "mez-curated-{}-{:x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&directory).unwrap();
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _directory = Directory(directory.clone());
    for path in [
        "home",
        "config",
        "work",
        "plugin",
        "plugin/.claude-plugin",
        "plugin/hooks",
    ] {
        std::fs::create_dir(directory.join(path)).unwrap();
    }
    let path = directory.join("probe.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    let control_path = directory.join("control.sock");
    let control_listener = if admission {
        Some(tokio::net::UnixListener::bind(&control_path).unwrap())
    } else {
        None
    };
    if let Some(listener) = &control_listener {
        crate::runtime::enable_unix_writer_credentials(listener.as_raw_fd()).unwrap();
    }
    let service = RuntimeServiceFixture::new()
        .control_socket(if admission { &control_path } else { &path })
        .build();
    struct Probe {
        service: RuntimeSessionService,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            let _ = self.service.terminate_all_pane_processes();
        }
    }
    let mut probe = Probe { service };
    start_probe_shell(&mut probe.service, &directory);
    std::fs::write(
        directory.join("plugin/.claude-plugin/plugin.json"),
        b"{\"name\":\"mez-native-source-probe\",\"version\":\"0.0.1\"}\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("plugin/hooks/hooks.json"),
        b"{\"modules\":[\"./register.mjs\"]}\n",
    )
    .unwrap();
    let argv = serde_json::json!([
        std::env::current_exe().unwrap(),
        "--exact",
        "runtime::control::external_enrollment::tests::claude_curated::external_claude_curated_command_helper_fixture",
        "--ignored",
        "--quiet"
    ]);
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    if admission {
        assert!(helper.is_file());
    }
    let source_call = if handoff {
        let successor = if owned {
            crate::integrations::bootstrap::curated_client::session_start_owned_successor_body(
                &helper,
                1000,
                "source-probe-module-b",
            )
            .unwrap()
        } else {
            crate::integrations::bootstrap::curated_client::session_start_successor_body(
                &helper,
                1000,
                "source-probe-module-b",
            )
            .unwrap()
        };
        let helper = serde_json::to_string(helper.to_str().unwrap()).unwrap();
        format!(
            "const admitted = await $.process.run([{helper}, 'harness-source', JSON.stringify({{ external_session_id: e.session_id, observer_instance: 'source-probe-module-a', session_boundary: e.source }})], {{ timeoutMs: 3000 }}); const previousObserver = Object.freeze(JSON.parse(admitted.stdout)); if (previousObserver.registered !== true || 'launch_token' in previousObserver || previousObserver.controls.length !== 0) throw new Error('original source unavailable'); {successor} const stale = await $.process.run([{helper}, 'harness-source', JSON.stringify({{ operation: 'curated-heartbeat', external_session_id: previousObserver.external_session_id, generation: previousObserver.generation, observer_witness: previousObserver.observer_witness, sequence: 1 }})], {{ timeoutMs: 3000 }}); if (JSON.parse(stale.stdout).observed !== false) throw new Error('stale epoch remained authoritative');"
        )
    } else if owned {
        crate::integrations::bootstrap::curated_client::session_start_owned_body(&helper, 1000)
            .unwrap()
    } else if proofs > 1 {
        crate::integrations::bootstrap::curated_client::session_start_body(&helper, 1000).unwrap()
    } else if admission {
        assert!(helper.is_file());
        format!(
            "const source = await $.process.run([{}, 'harness-source', JSON.stringify({{ external_session_id: e.session_id, observer_instance: 'source-probe-module-a', session_boundary: e.source }})], {{ timeoutMs: 3000 }}); const publicResult = JSON.parse(source.stdout); if (publicResult.registered !== true || 'launch_token' in publicResult || publicResult.controls.length !== 0) throw new Error('source probe unavailable');",
            serde_json::to_string(helper.to_str().unwrap()).unwrap()
        )
    } else {
        String::new()
    };
    let schedule = if proofs == 1 {
        format!(
            "const original = Object.freeze({{ external_session_id: publicResult.external_session_id, generation: publicResult.generation, observer_witness: publicResult.observer_witness }}); $.clock.after(1000, async () => {{ try {{ const proof = await $.process.run([{}, 'harness-source', JSON.stringify({{ operation: 'curated-heartbeat', external_session_id: original.external_session_id, generation: original.generation, observer_witness: original.observer_witness, sequence: 1 }})], {{ timeoutMs: 3000 }}); const receipt = JSON.parse(proof.stdout); if (receipt.observed !== true || 'launch_token' in receipt) throw new Error('observer proof unavailable'); }} catch {{ /* unavailable proof cannot change vendor results */ }} }});",
            serde_json::to_string(helper.to_str().unwrap()).unwrap()
        )
    } else {
        String::new()
    };
    let hold = if node_shim {
        String::new()
    } else {
        format!(
            "await $.process.run({argv}, {{ env: {{ MEZ_TEST_CURATED_SOCKET: {} }}, timeoutMs: 10000 }});",
            serde_json::to_string(path.to_str().unwrap()).unwrap()
        )
    };
    let (import, create, duplicate, stop) = if owned {
        std::fs::write(
            directory.join("plugin/hooks/curated_lifetime.mjs"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/integrations/bootstrap/curated_lifetime.mjs"
            )),
        )
        .unwrap();
        let duplicate = if handoff {
            crate::integrations::bootstrap::curated_client::session_start_owned_successor_body(
                &helper,
                1000,
                "source-probe-module-b",
            )
            .unwrap()
        } else {
            crate::integrations::bootstrap::curated_client::session_start_owned_body(&helper, 1000)
                .unwrap()
        };
        (
            "import { createCuratedLifetime } from './curated_lifetime.mjs';",
            "const observerLifetime = createCuratedLifetime();",
            duplicate,
            "observerLifetime.stop(); if (observerLifetime.receipt !== undefined) throw new Error('observer lifetime remained active');",
        )
    } else {
        ("", "", String::new(), "")
    };
    let source = if owned && proofs == 5 {
        std::fs::write(
            directory.join("plugin/hooks/claude_observer.mjs"),
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/integrations/bootstrap/claude_observer.mjs"
            )),
        )
        .unwrap();
        let module =
            crate::integrations::bootstrap::curated_client::module_source(&helper, 1000).unwrap();
        let marker = "    return result;\n  });\n\n  on('classic.SessionEnd'";
        assert_eq!(module.matches(marker).count(), 1);
        module.replace(
            marker,
            &format!("    {hold}\n    return result;\n  }});\n\n  on('classic.SessionEnd'"),
        )
    } else if owned && proofs == 4 {
        format!(
            "{import} export function register(on) {{ {create} let setupTicket; on('classic.Setup', async ($, e, next) => {{ const result = await next(e); setupTicket = observerLifetime.begin('module-scope-probe'); if (!setupTicket) throw new Error('module scope unavailable'); return result; }}); on('classic.SessionStart', async ($, e, next) => {{ const result = await next(e); if (observerLifetime.begin('module-scope-probe') !== undefined || !observerLifetime.release(setupTicket)) throw new Error('module scope lost across callbacks'); {source_call} {duplicate} {schedule} {hold} {stop} return result; }}); }}\n"
        )
    } else {
        format!(
            "{import} export function register(on) {{ on('classic.SessionStart', async ($, e, next) => {{ {create} const result = await next(e); {source_call} {duplicate} {schedule} {hold} {stop} return result; }}); }}\n"
        )
    };
    std::fs::write(directory.join("plugin/hooks/register.mjs"), source).unwrap();
    let quote = |path: &std::path::Path| {
        shlex::try_quote(path.to_str().unwrap())
            .unwrap()
            .into_owned()
    };
    let invocation = if node_shim {
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/curated-client-ordinary-fixture.mjs")
            .canonicalize()
            .unwrap();
        format!(
            "{} {} {} {} {} {}",
            quote(&vendor),
            quote(&script),
            quote(&directory.join("plugin/hooks/register.mjs")),
            quote(&std::env::current_exe().unwrap()),
            quote(&path),
            quote(&directory.join("callback-returned"))
        )
    } else {
        format!(
            "{} --init-only --setting-sources '' --plugin-dir {}",
            quote(&vendor),
            quote(&directory.join("plugin"))
        )
    };
    let command = format!(
        "cd {} && /usr/bin/env -i PATH=/usr/bin:/bin HOME={} CLAUDE_CONFIG_DIR={} MEZ=\"$MEZ\" MEZ_PANE=\"$MEZ_PANE\" {invocation}; printf '%s\\n' \"$?\" > {}\n",
        quote(&directory.join("work")),
        quote(&directory.join("home")),
        quote(&directory.join("config")),
        quote(&directory.join("vendor-exit"))
    );
    probe
        .service
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let actor = if let Some(control_listener) = control_listener {
        let service = std::mem::replace(&mut probe.service, RuntimeServiceFixture::new().build());
        let (handle, actor) =
            AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
        let task = tokio::spawn(actor.run());
        let caller = handle.clone();
        let callback_report = directory.join("callback-returned");
        let server = tokio::spawn(async move {
            for ordinal in 0..1 + proofs + if handoff { 2 } else { 0 } {
                let (mut stream, _) =
                    tokio::time::timeout(Duration::from_secs(15), control_listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                if node_shim && ordinal > 0 {
                    assert_eq!(
                        std::fs::read(&callback_report).unwrap(),
                        b"returned",
                        "proof arrived before callback return"
                    );
                }
                let mut connection = ControlConnectionState::new(true, false);
                serve_async_runtime_control_connection_loop(
                    &mut stream,
                    &caller,
                    &mut connection,
                    AsyncRuntimeControlConnectionConfig::new(
                        8192,
                        crate::runtime::current_effective_uid(),
                    )
                    .unwrap(),
                    |_, _| false,
                )
                .await
                .unwrap();
                assert!(!connection.initialized());
                assert!(connection.caller_client_id().is_none());
            }
        });
        Some((handle, task, server))
    } else {
        None
    };
    let accepted = tokio::time::timeout(Duration::from_secs(20), listener.accept()).await;
    if let Some((handle, task, server)) = actor {
        // Recover pane ownership even if the SDK/helper fails before the hold
        // probe connects; assertions below then run under the cleanup guard.
        let server_result = if accepted.as_ref().is_ok_and(|result| result.is_ok()) {
            Some(server.await)
        } else {
            server.abort();
            None
        };
        let shutdown = handle.shutdown().await;
        probe.service = task.await.unwrap().service;
        shutdown.unwrap();
        if let Some(result) = server_result {
            result.unwrap();
        }
        assert_eq!(probe.service.control.external_agents().bindings.len(), 1);
    }
    let (mut socket, _) = accepted
        .expect("isolated init-only mod did not invoke curated process helper")
        .unwrap();
    let uid = crate::runtime::current_effective_uid();
    let origin = Arc::new(
        crate::runtime::capture_unix_origin(socket.as_raw_fd(), uid)
            .expect("native source probe needs supported kernel peer lifetime"),
    );
    let mut qualified = crate::runtime::UnixOriginStream::new(&mut socket, Some(origin.clone()));
    let mut ready = [0];
    use tokio::io::AsyncReadExt;
    tokio::time::timeout(Duration::from_secs(10), qualified.read_exact(&mut ready))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ready, [1]);
    assert!(origin.writer_confirmed());
    drop(qualified);
    if node_shim {
        assert_eq!(
            std::fs::read(directory.join("callback-returned")).unwrap(),
            b"returned"
        );
    }
    let root = probe.service.pane_process_identity("%1").unwrap();
    let root_record = mez_mux::process::process_parent_identity_for_pid(root.process_id).unwrap();
    assert_eq!(root_record.start_token, root.start_token);
    let worker_origin = origin.clone();
    let parent = tokio::task::spawn_blocking(move || worker_origin.capture_parent())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(parent.identity.process_id, root.process_id);
    assert_ne!(parent.identity.process_id, origin.identity.process_id);
    let executable =
        std::fs::read_link(format!("/proc/{}/exe", parent.identity.process_id)).unwrap();
    assert_eq!(
        executable,
        std::fs::canonicalize(&vendor).unwrap(),
        "curated helper parent is not the expected local vendor process"
    );
    if admission {
        let binding = probe
            .service
            .control
            .external_agents()
            .bindings
            .values()
            .next()
            .unwrap();
        let owner = binding.enrollment.as_ref().unwrap();
        assert!(owner.producer.matches_parent(parent.uid(), parent.identity));
        assert!(owner.observers.is_empty());
        assert_eq!(owner.has_live_observer(), proofs > 0);
        assert_eq!(binding.harness, "claude");
        if owned && proofs == 5 {
            assert_eq!(owner.epoch, 1);
            assert_eq!(owner.instance, "mez-curated-client-1");
        }
        if handoff {
            assert_eq!(owner.epoch, 2);
            assert_eq!(owner.instances.len(), 2);
            assert_eq!(owner.instance, "source-probe-module-b");
            assert_ne!(binding.generation, owner.run_generation);
        }
        if proofs > 0 {
            assert_eq!(owner.curated_observer.as_ref().unwrap().sequence, proofs);
            let original_expiry = binding.expires;
            assert_eq!(binding.registration.as_ref().unwrap().presentation, None);
            probe
                .service
                .control
                .external_agents_mut()
                .bindings
                .values_mut()
                .next()
                .unwrap()
                .expires = current_unix_seconds() - 1;
            probe.service.renew_connected_external_observers();
            assert!(
                probe
                    .service
                    .control
                    .external_agents()
                    .bindings
                    .values()
                    .next()
                    .unwrap()
                    .expires
                    >= original_expiry
            );
            if node_shim {
                let registry = probe.service.control.external_agents_mut();
                let binding = registry.bindings.values_mut().next().unwrap();
                let owner = binding.enrollment.as_mut().unwrap();
                owner.curated_observer.as_mut().unwrap().observed_at =
                    Some(Instant::now() - Duration::from_secs(31));
                binding.expires = current_unix_seconds() - 1;
                // Simulated proof loss exercises daemon policy without a long
                // wall-clock test; the real native creator remains alive.
                probe.service.renew_connected_external_observers();
                probe.service.reconcile_external_agent_registrations();
                let binding = probe
                    .service
                    .control
                    .external_agents()
                    .bindings
                    .values()
                    .next()
                    .unwrap();
                assert!(binding.retired);
                assert!(binding.enrollment.as_ref().unwrap().producer.is_live());
                assert!(
                    !probe
                        .service
                        .control
                        .external_agents()
                        .enrollments
                        .curated_namespaces
                        .is_empty()
                );
            }
        }
    }
    let budget = Arc::new(UnixAncestryBudget::default());
    let worker_budget = budget.clone();
    let (parent, ancestry) = tokio::task::spawn_blocking(move || {
        let ancestry = parent
            .capture_ancestry(root_record, &worker_budget)
            .unwrap();
        (parent, ancestry)
    })
    .await
    .unwrap();
    assert!(ancestry.source_matches(parent.uid(), parent.identity));
    assert!(ancestry.is_live());
    socket.write_all(&[2]).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while parent.is_live() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("offline producer did not exit after helper completion");
    assert!(!origin.is_live());
    assert!(!ancestry.is_live());
    assert_eq!(
        mez_mux::process::process_parent_identity_for_pid(root.process_id)
            .unwrap()
            .start_token,
        root.start_token,
        "surviving pane shell must not keep dead producer telemetry alive"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if std::fs::read(directory.join("vendor-exit")).is_ok_and(|bytes| bytes == b"0\n") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("offline producer did not publish successful exit");
    drop(ancestry);
    assert_eq!(budget.reserved(), 0);
    if admission {
        probe.service.reconcile_external_agent_registrations();
        assert!(
            probe
                .service
                .control
                .external_agents()
                .bindings
                .values()
                .all(|binding| binding.retired)
        );
        assert!(
            probe
                .service
                .control
                .external_agents()
                .enrollments
                .curated_namespaces
                .is_empty()
        );
    } else {
        assert!(probe.service.control.external_agents().bindings.is_empty());
    }
    assert!(
        probe
            .service
            .control
            .external_agents()
            .enrollments
            .pending
            .is_empty()
    );
    probe.service.terminate_all_pane_processes().unwrap();
}
