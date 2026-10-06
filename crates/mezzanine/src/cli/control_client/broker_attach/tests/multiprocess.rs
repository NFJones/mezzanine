//! Combined real-host, broker-process and independently attached CLI qualification.
//!
//! Disposable roots retain the same paired identity and durable trust. PTY input
//! executes only fixed harmless printf commands; no provider or desktop clipboard
//! work occurs. This test proves automated terminal bytes, not physical UX or X11.

use super::*;
use crate::host::iroh::HostIrohRuntime;
use crate::host::router::{HostDefaultSessionPolicy, HostSessionRouter, HostSessionRouterConfig};
use crate::host::shell::{ResolvedShell, ShellSource};
use crate::security::remote::{
    RemoteHostRoutingAuthority, RemoteSessionAttachScope, RemoteTrustStore,
};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

/// One actual CLI terminal, retaining process and I/O cleanup on assertion failure.
struct CliTerminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Option<Box<dyn Write + Send>>,
    output: std::sync::mpsc::Receiver<Vec<u8>>,
    reader: Option<std::thread::JoinHandle<()>>,
    observed: Vec<u8>,
    screen: mez_terminal::TerminalScreen,
}

impl CliTerminal {
    /// Starts an ordinary new invocation, not a direct call to session setup.
    fn spawn(executable: &Path, home: &Path, runtime: &Path, name: &str) -> Result<Self> {
        let pair = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|_| MezError::invalid_state("fixture PTY unavailable"))?;
        let mut command = portable_pty::CommandBuilder::new(executable);
        command.env_clear();
        command.env("HOME", home);
        command.env("SHELL", "/bin/sh");
        command.env("MEZ_TMPDIR", runtime);
        command.env("TERM", "xterm-256color");
        command.args(["--iroh-profile", "fixture", "new", "--name", name]);
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|_| MezError::invalid_state("fixture CLI launch unavailable"))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|_| MezError::invalid_state("fixture PTY reader unavailable"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|_| MezError::invalid_state("fixture PTY writer unavailable"))?;
        drop(pair.slave);
        let (sender, output) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut bytes = [0; 4096];
            while let Ok(count) = reader.read(&mut bytes) {
                if count == 0 || sender.send(bytes[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            _master: pair.master,
            writer: Some(writer),
            output,
            reader: Some(reader),
            observed: Vec::new(),
            screen: mez_terminal::TerminalScreen::new(
                mez_terminal::TerminalSize::new(80, 24).unwrap(),
                100,
            )
            .unwrap(),
        })
    }

    /// Waits asynchronously for fixed fixture output while host services progress.
    /// Retention is finite and diagnostics never expose terminal/session content.
    async fn wait_text(&mut self, expected: &str) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            while let Ok(bytes) = self.output.try_recv() {
                if self.observed.len().saturating_add(bytes.len()) > 1024 * 1024 {
                    return Err(MezError::invalid_state(
                        "fixture terminal output exceeds limit",
                    ));
                }
                self.screen.feed(&bytes);
                self.observed.extend_from_slice(&bytes);
            }
            if self
                .screen
                .visible_lines()
                .iter()
                .any(|row| row.contains(expected))
            {
                return Ok(());
            }
            if self.child.try_wait()?.is_some() {
                return Err(MezError::invalid_state(
                    "fixture CLI exited before expected output",
                ));
            }
            if Instant::now() >= deadline {
                return Err(MezError::invalid_state(format!(
                    "fixture CLI output timed out waiting for fixed predicate {expected}; observed_bytes={}",
                    self.observed.len()
                )));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Sends a fixed command; its expected result is not present in the echo.
    fn input(&mut self, command: &[u8]) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| MezError::invalid_state("fixture input closed"))?;
        writer.write_all(command)?;
        writer.flush()?;
        Ok(())
    }

    /// Signals and reaps this exact foreground child, not the broker or a sibling.
    async fn interrupt_and_reap(&mut self) -> Result<()> {
        let pid = self
            .child
            .process_id()
            .and_then(|pid| i32::try_from(pid).ok())
            .and_then(rustix::process::Pid::from_raw)
            .ok_or_else(|| MezError::invalid_state("fixture CLI process identity unavailable"))?;
        rustix::process::kill_process(pid, rustix::process::Signal::INT)
            .map_err(std::io::Error::from)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err(MezError::invalid_state("fixture CLI shutdown timed out"));
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }
}

impl Drop for CliTerminal {
    fn drop(&mut self) {
        self.writer.take();
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Real authorizing host + real shared broker + two ordinary new CLI processes
/// must create distinct leases under the existing paired endpoint. Each terminal
/// executes input independently, and the second remains usable after the first
/// exits. Explicit teardown releases the endpoint lock without killing sessions
/// on frontend disconnect. Trust is seeded through the protected test store,
/// rather than pretending this also qualifies invitation pairing or X11.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for combined process qualification"]
async fn broker_attach_real_host_two_cli_terminals_preserve_sibling_input() {
    let executable = PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit trusted executable required"),
    );
    assert!(executable.is_absolute());
    let root = std::env::temp_dir().join(format!("mez-combined-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    let env = crate::cli::CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let cli_runtime = root.join("cli-runtime");
    std::fs::create_dir(&cli_runtime).unwrap();
    std::fs::set_permissions(&cli_runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = crate::runtime::RuntimeIrohTransportPolicy {
        compression_codecs: vec![crate::runtime::RuntimeIrohCompressionCodec::None],
        ..Default::default()
    };
    std::fs::write(
        paths.default_primary_file(),
        format!(
            "version = {}\n[transport.iroh]\ncompression_codecs = [\"none\"]\n",
            crate::config::CURRENT_CONFIG_SCHEMA_VERSION,
        ),
    )
    .unwrap();
    let host_root = root.join("host");
    let host = HostIrohRuntime::bind(
        &host_root,
        crate::runtime::RuntimeIrohTransportPolicy {
            enabled: true,
            ..policy.clone()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let identity = RemoteClientIdentity::load_or_create(paths.root()).unwrap();
    let endpoint_id = identity.endpoint_id().to_string();
    drop(identity);
    let trust = RemoteTrustStore::under_host_config_root(&host_root).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let invitation = trust
        .create_host_invitation(
            host.endpoint_id(),
            RemoteRoleCeiling::Primary,
            RemoteHostRoutingAuthority {
                session_create: true,
                session_kill: true,
                session_list: true,
                session_attach_scope: RemoteSessionAttachScope::Own,
                max_active_leases: 4,
                max_live_sessions: 4,
                lease_lifetime_ceiling_seconds: None,
            },
            600,
            now,
        )
        .unwrap();
    let redemption = trust
        .redeem_invitation(
            &invitation.token,
            host.endpoint_id(),
            &endpoint_id,
            "fixture",
            crate::control::RequestedRole::Primary,
            now,
        )
        .unwrap();
    RemoteClientProfileStore::under_config_root(paths.root())
        .save(&RemoteClientProfile {
            name: "fixture".into(),
            server_addr: host.endpoint_addr().unwrap(),
            role: RemoteRoleCeiling::Primary,
            scope: RemoteClientProfileScope::Host,
            device_credential: redemption.device_credential,
        })
        .unwrap();
    let router = HostSessionRouter::new(HostSessionRouterConfig {
        runtime_root: root.join("host-runtime"),
        owner_uid: crate::runtime::current_effective_uid(),
        config_root: host_root.clone(),
        config_layers: vec![],
        shell: ResolvedShell::new(PathBuf::from("/bin/sh"), ShellSource::FallbackBinSh),
        max_sessions: 4,
        max_live_sessions: 4,
        default_session_policy: HostDefaultSessionPolicy::MostRecentAttachable,
        default_lease_lifetime_seconds: 0,
    });
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let host_stop = stop.clone();
    let serve = host.serve_routed(router.clone(), async move { host_stop.notified().await });
    let mut broker_child = None;
    let work = async {
        let ready = crate::cli::remote::broker::launch::connect_owned(
            &executable,
            &env,
            policy.setup_timeout,
            &mut broker_child,
        )
        .await?;
        drop(ready);
        let mut first = CliTerminal::spawn(&executable, &home, &cli_runtime, "combined-first")?;
        first.wait_text("□").await?;
        first.input(b"printf 'FIRST-%s\\n' 'LIVE'\n")?;
        first.wait_text("FIRST-LIVE").await?;
        let mut second = CliTerminal::spawn(&executable, &home, &cli_runtime, "combined-second")?;
        second.wait_text("□").await?;
        second.input(b"printf 'SECOND-%s\\n' 'LIVE'\n")?;
        second.wait_text("SECOND-LIVE").await?;
        let management = crate::host::outbound_frontend::client::OutboundFrontendClient::connect(
            paths.root(),
            policy.setup_timeout,
        )
        .await?;
        let leases = serde_json::to_value(
            management
                .list_sessions("fixture", policy.setup_timeout)
                .await?,
        )
        .unwrap();
        let leases = leases.as_array().unwrap();
        assert_eq!(leases.len(), 2);
        assert_ne!(leases[0]["session_id"], leases[1]["session_id"]);
        assert_ne!(leases[0]["lease_id"], leases[1]["lease_id"]);
        assert_eq!(router.snapshots().await?.len(), 2);
        assert!(first.child.try_wait()?.is_none());
        first.interrupt_and_reap().await?;
        second.input(b"printf 'SIBLING-%s\\n' 'SURVIVES'\n")?;
        second.wait_text("SIBLING-SURVIVES").await?;
        assert!(second.child.try_wait()?.is_none());
        second.interrupt_and_reap().await?;
        assert_eq!(
            router.snapshots().await?.len(),
            2,
            "frontend exit must preserve committed sessions"
        );
        assert!(
            RemoteClientIdentity::load_or_create(paths.root()).is_err(),
            "broker retains exclusive identity"
        );
        Ok::<(), MezError>(())
    };
    let work = async {
        let result = tokio::time::timeout(Duration::from_secs(60), Box::pin(work)).await;
        stop.notify_one();
        result
    };
    let (served, result) = tokio::join!(serve, work);
    let shutdown = if let Some(child) = broker_child.as_mut() {
        tokio::time::timeout(Duration::from_secs(10), child.shutdown_for_tests()).await
    } else {
        Ok(Err(MezError::invalid_state("fixture broker child missing")))
    };
    if let Some(child) = broker_child.as_mut()
        && child.try_wait().unwrap().is_none()
    {
        tokio::time::timeout(Duration::from_secs(5), child.terminate_for_tests())
            .await
            .unwrap()
            .unwrap();
    }
    router
        .shutdown_all(true, Duration::from_secs(5))
        .await
        .unwrap();
    served.unwrap();
    result.expect("combined workflow must finish").unwrap();
    assert!(shutdown.unwrap().unwrap().success());
    assert!(!paths.root().join("outbound.sock").exists());
    let identity = RemoteClientIdentity::load_or_create(paths.root()).unwrap();
    assert_eq!(
        identity.endpoint_id().to_string(),
        endpoint_id,
        "paired principal must remain unchanged"
    );
    drop(identity);
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
