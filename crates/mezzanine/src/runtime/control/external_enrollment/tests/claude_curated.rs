//! Opt-in real curated-command creator evidence without a provider conversation.
//!
//! An owned temporary mod uses literal classic.SessionStart/next/process.run
//! calls. --init-only runs Setup/SessionStart then exits. HOME/config/cwd/env are
//! isolated and no credentials/prompt, installation, policy bypass, supplied PID
//! or enrollment capability exists. This qualifies only a source relationship,
//! not a long-lived producer, enabled integration or authorization contract.

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
    let vendor = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_CLAUDE_BINARY").expect("explicit installed Claude executable"),
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
    let service = RuntimeServiceFixture::new().control_socket(&path).build();
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
    let source = format!(
        "export function register(on) {{ on('classic.SessionStart', async ($, e, next) => {{ const result = await next(e); await $.process.run({argv}, {{ env: {{ MEZ_TEST_CURATED_SOCKET: {} }}, timeoutMs: 10000 }}); return result; }}); }}\n",
        serde_json::to_string(path.to_str().unwrap()).unwrap()
    );
    std::fs::write(directory.join("plugin/hooks/register.mjs"), source).unwrap();
    let quote = |path: &std::path::Path| {
        shlex::try_quote(path.to_str().unwrap())
            .unwrap()
            .into_owned()
    };
    let command = format!(
        "cd {} && /usr/bin/env -i PATH=/usr/bin:/bin HOME={} CLAUDE_CONFIG_DIR={} {} --init-only --setting-sources '' --plugin-dir {}; printf '%s\\n' \"$?\" > {}\n",
        quote(&directory.join("work")),
        quote(&directory.join("home")),
        quote(&directory.join("config")),
        quote(&vendor),
        quote(&directory.join("plugin")),
        quote(&directory.join("vendor-exit"))
    );
    probe
        .service
        .write_runtime_pane_input("%1", command.as_bytes())
        .unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(20), listener.accept())
        .await
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
    .expect("init-only vendor did not exit after helper completion");
    assert!(!origin.is_live());
    assert!(!ancestry.is_live());
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if std::fs::read(directory.join("vendor-exit")).is_ok_and(|bytes| bytes == b"0\n") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("init-only vendor did not publish successful exit");
    drop(ancestry);
    assert_eq!(budget.reserved(), 0);
    assert!(probe.service.control.external_agents().bindings.is_empty());
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
