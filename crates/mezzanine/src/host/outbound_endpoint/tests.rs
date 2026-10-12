//! Local-only transport lifetime checks; no application authority or sessions.
//!
//! Loopback peers prove independent QUIC connection ownership under one locked
//! persistent identity. Synthetic payloads carry no user configuration secrets.

use super::*;
use std::os::unix::fs::PermissionsExt;

/// Frontend discovery must reject a relocated root without recreating the old
/// pathname, and a replacement private directory cannot inherit the retained
/// endpoint's identity. Restoring the original object permits discovery again.
#[tokio::test]
async fn outbound_endpoint_root_revalidation_is_read_only_and_object_scoped() {
    let root =
        std::env::temp_dir().join(format!("mez-outbound-root-{:032x}", rand::random::<u128>()));
    let moved = root.with_extension("moved");
    let owner = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    assert_eq!(owner.frontend_config_root().unwrap(), root);
    std::fs::rename(&root, &moved).unwrap();
    assert!(owner.frontend_config_root().is_err());
    assert!(
        !root.exists(),
        "revalidation must not recreate a missing root"
    );
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(owner.frontend_config_root().is_err());
    std::fs::remove_dir(&root).unwrap();
    std::fs::rename(&moved, &root).unwrap();
    assert_eq!(owner.frontend_config_root().unwrap(), root);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(owner.frontend_config_root().is_err());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    owner.begin_shutdown().unwrap().finish().await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

/// Isolated process fixture for intentionally quarantined lock descriptors.
/// It publishes only a static readiness marker after abandoning ownership and
/// waits for exact parent input, keeping the fail-closed descriptor process-scoped.
/// Explicit exit then qualifies OS death, not unrelated Tokio runtime teardown.
/// Optional controls run only in this exact ignored private subprocess fixture.
#[tokio::test]
#[ignore = "subprocess helper for outbound endpoint quarantine regression"]
async fn outbound_endpoint_quarantine_child() {
    use std::io::{Read, Write};
    let root = std::path::PathBuf::from(std::env::var_os("MEZ_OUTBOUND_TEST_ROOT").unwrap());
    let mode = std::env::var("MEZ_OUTBOUND_TEST_MODE").unwrap();
    let behavior = std::env::var("MEZ_OUTBOUND_TEST_BEHAVIOR").unwrap();
    quarantine_marker("OUTBOUND_QUARANTINE_BINDING");
    let owner = OutboundEndpointOwner::bind(&root, &RuntimeIrohTransportPolicy::default())
        .await
        .unwrap();
    quarantine_marker("OUTBOUND_QUARANTINE_BOUND");
    if mode == "shutdown" {
        let mut shutdown = owner.begin_shutdown().unwrap();
        shutdown.work = Some(Box::pin(std::future::pending()));
        let mut wait = Box::pin(shutdown.finish());
        assert!(matches!(
            futures_util::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        drop(wait);
        drop(shutdown);
    } else {
        assert_eq!(mode, "owner");
        drop(owner);
    }
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    if behavior == "held-runtime" {
        // A real blocking worker has entered before READY. Its channel sender
        // intentionally survives until OS death, even if this test returns;
        // runtime Drop would otherwise wait forever for the held worker.
        let (hold, blocked) = std::sync::mpsc::channel::<()>();
        let (entered, entry) = tokio::sync::oneshot::channel();
        tokio::task::spawn_blocking(move || {
            entered.send(()).unwrap();
            let _ = blocked.recv();
        });
        std::mem::forget(hold);
        entry.await.unwrap();
    }
    if behavior == "early-release" {
        quarantine_marker("OUTBOUND_QUARANTINE_RELEASED");
        std::process::exit(0);
    }
    quarantine_marker("OUTBOUND_QUARANTINE_READY");
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    if byte != *b"x" {
        std::process::exit(64);
    }
    if behavior == "missing-release" {
        std::process::exit(0);
    }
    quarantine_marker("OUTBOUND_QUARANTINE_RELEASED");
    std::io::stdout().flush().unwrap();
    if behavior == "failed-exit" {
        std::process::exit(65);
    }
    // The quarantine contract releases authority only on actual process death.
    // Returning from tokio::test adds runtime shutdown as an unintended gate;
    // no graceful teardown or production behavior is claimed by this fixture.
    std::process::exit(0);
}

/// Flush a static content-free stage; never publish paths, keys or panic output.
fn quarantine_marker(marker: &'static str) {
    use std::io::Write;
    println!("{marker}");
    std::io::stdout().flush().unwrap();
}

/// Own a child with kill-on-drop fallback and one total protocol deadline.
/// Diagnostics contain only known stage labels, I/O error kinds and exit status;
/// arbitrary child stdout/stderr is neither echoed nor retained in error text.
struct QuarantineProcess {
    child: tokio::process::Child,
    output: tokio::io::BufReader<tokio::process::ChildStdout>,
    deadline: tokio::time::Instant,
    marker: &'static str,
}

impl QuarantineProcess {
    /// Spawn only the exact ignored child against a synthetic private identity.
    fn spawn(root: &Path, mode: &str, behavior: &str) -> std::result::Result<Self, String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        let executable =
            std::env::current_exe().map_err(|error| format!("spawn: {:?}", error.kind()))?;
        let mut child = tokio::process::Command::new(executable)
            .args([
                "host::outbound_endpoint::tests::outbound_endpoint_quarantine_child",
                "--exact",
                "--ignored",
                "--nocapture",
                "--format=terse",
                "--test-threads=1",
            ])
            .env_clear()
            .env("MEZ_OUTBOUND_TEST_ROOT", root)
            .env("MEZ_OUTBOUND_TEST_MODE", mode)
            .env("MEZ_OUTBOUND_TEST_BEHAVIOR", behavior)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn: {:?}", error.kind()))?;
        let output = tokio::io::BufReader::new(child.stdout.take().unwrap());
        Ok(Self {
            child,
            output,
            deadline,
            marker: "spawned",
        })
    }

    /// Attach content-free last-stage and liveness evidence to a protocol error.
    fn diagnostic(&mut self, stage: &str, reason: &str) -> String {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("{status}"),
            Ok(None) => "live".to_owned(),
            Err(error) => format!("unavailable({:?})", error.kind()),
        };
        format!(
            "quarantine {stage}: {reason}; last={}; child={status}",
            self.marker
        )
    }

    /// Require ordered known markers while discarding bounded libtest prelude.
    /// A line cannot exceed 256 bytes, nor can more than 16 prelude lines hide
    /// an absent marker. Raw output never becomes a diagnostic, including errors.
    async fn marker(
        &mut self,
        expected: &'static str,
        stage: &str,
    ) -> std::result::Result<(), String> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let read = async {
            for _ in 0..16 {
                let mut line = Vec::new();
                (&mut self.output)
                    .take(256)
                    .read_until(b'\n', &mut line)
                    .await
                    .map_err(|error| format!("read {:?}", error.kind()))?;
                if line.is_empty() {
                    return Err("EOF before marker".to_owned());
                }
                if !line.ends_with(b"\n") {
                    return Err("overlong protocol line".to_owned());
                }
                if line == format!("{expected}\n").as_bytes() {
                    return Ok(());
                }
                if line.starts_with(b"OUTBOUND_QUARANTINE_") {
                    return Err("out-of-order marker".to_owned());
                }
            }
            Err("excess protocol prelude".to_owned())
        };
        match tokio::time::timeout_at(self.deadline, read).await {
            Ok(Ok(())) => {
                self.marker = expected;
                Ok(())
            }
            Ok(Err(reason)) => Err(self.diagnostic(stage, &reason)),
            Err(_) => Err(self.diagnostic(stage, "deadline elapsed")),
        }
    }

    /// Verify live exclusion, send exact release, require acknowledgement and
    /// actual successful child exit under the same original deadline. The read
    /// side is drained concurrently with wait up to a finite wrapper-tail bound;
    /// excess output fails qualification, potentially at the shared deadline.
    async fn qualify(&mut self, root: &Path, release: u8) -> std::result::Result<(), String> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        self.marker("OUTBOUND_QUARANTINE_BINDING", "binding")
            .await?;
        self.marker("OUTBOUND_QUARANTINE_BOUND", "bound").await?;
        self.marker("OUTBOUND_QUARANTINE_READY", "readiness")
            .await?;
        match self.child.try_wait() {
            Ok(None) => {}
            _ => return Err(self.diagnostic("live exclusion", "child not live")),
        }
        let error = RemoteClientIdentity::load_or_create(root).err();
        if !error.is_some_and(|error| error.kind() == MezErrorKind::Conflict) {
            return Err(self.diagnostic("live exclusion", "identity not exclusively quarantined"));
        }
        let mut input = self.child.stdin.take().unwrap();
        match tokio::time::timeout_at(self.deadline, input.write_all(&[release])).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return Err(self.diagnostic("release", &format!("write {:?}", error.kind())));
            }
            Err(_) => return Err(self.diagnostic("release", "deadline elapsed")),
        }
        drop(input);
        self.marker("OUTBOUND_QUARANTINE_RELEASED", "release acknowledgement")
            .await?;
        let exit = async {
            // Only a bounded wrapper tail is permitted. Limit + 1 detects excess
            // without retaining/printing output or allowing unbounded allocation.
            let mut drain = (&mut self.output).take(4097);
            let mut sink = tokio::io::sink();
            let (bytes, status) =
                tokio::join!(tokio::io::copy(&mut drain, &mut sink), self.child.wait());
            let bytes = bytes.map_err(|error| format!("drain {:?}", error.kind()))?;
            let status = status.map_err(|error| format!("wait {:?}", error.kind()))?;
            if bytes > 4096 {
                return Err("excess protocol tail".to_owned());
            }
            if !status.success() {
                return Err(format!("unsuccessful {status}"));
            }
            Ok(())
        };
        match tokio::time::timeout_at(self.deadline, exit).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(self.diagnostic("process exit", &reason)),
            Err(_) => Err(self.diagnostic("process exit", "deadline elapsed")),
        }
    }

    /// Reap a failed fixture after kill; kill-on-drop remains the unwind fallback.
    /// A separate one-second failure-only cleanup bound cannot turn failure into
    /// success or widen the original 20-second qualification budget. Failure to
    /// reap preserves the original diagnostic and leaves kill-on-drop armed.
    async fn kill_and_reap(&mut self) -> std::result::Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => self
                .child
                .start_kill()
                .map_err(|error| format!("kill {:?}", error.kind()))?,
            Err(error) => return Err(format!("status {:?}", error.kind())),
        }
        match tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(format!("reap {:?}", error.kind())),
            Err(_) => Err("reap deadline elapsed".to_owned()),
        }
    }
}

/// Run one private process qualification, retaining the original public identity
/// for post-death comparison. Returned errors kill/reap before identity reuse;
/// failed cleanup never assumes death or removes a still-owned fixture root.
async fn quarantine_case(
    mode: &str,
    behavior: &str,
    release: u8,
) -> std::result::Result<(), String> {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-quarantine-{:032x}",
        rand::random::<u128>()
    ));
    let original = RemoteClientIdentity::load_or_create(&root).unwrap();
    let identity = original.endpoint_id();
    drop(original);
    let mut process = match QuarantineProcess::spawn(&root, mode, behavior) {
        Ok(process) => process,
        Err(error) => {
            std::fs::remove_dir_all(root).unwrap();
            return Err(error);
        }
    };
    let result = process.qualify(&root, release).await;
    if let Err(error) = &result
        && let Err(cleanup) = process.kill_and_reap().await
    {
        return Err(format!("{error}; cleanup={cleanup}"));
    }
    let restored = RemoteClientIdentity::load_or_create(&root).unwrap();
    assert_eq!(restored.endpoint_id(), identity);
    drop(restored);
    std::fs::remove_dir_all(root).unwrap();
    result
}

/// Last-owner disposal and abandoned shutdown must not release the key while
/// their process survives. Only process exit releases quarantined ownership;
/// the same persisted public identity then becomes safely acquirable again.
#[tokio::test]
async fn outbound_endpoint_abandoned_resources_quarantine_until_process_exit() {
    for mode in ["owner", "shutdown"] {
        quarantine_case(mode, "normal", b'x')
            .await
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
    }
}

/// An entered blocking runtime worker is deliberately held until process death.
/// Both real quarantine modes must still exit after exact parent release. Removing
/// explicit OS exit makes this fixture return through runtime Drop and deterministically
/// times out at process exit, without attributing the historical failure to that stage.
#[tokio::test]
async fn outbound_endpoint_quarantine_process_exit_does_not_wait_for_runtime_drop() {
    for mode in ["owner", "shutdown"] {
        quarantine_case(mode, "held-runtime", b'x')
            .await
            .unwrap_or_else(|error| panic!("{mode}: {error}"));
    }
}

/// Actual child negative controls cannot pass merely by exiting with status zero.
/// Early/out-of-order or missing release acknowledgement rejects at the protocol
/// boundary; nonzero exit after acknowledgement rejects at actual process wait;
/// a byte other than the exact parent release is also rejected. All cases preserve
/// live quarantine checks, kill/reap on error and the same persisted public identity.
#[tokio::test]
async fn outbound_endpoint_quarantine_protocol_rejects_early_release_and_failed_exit() {
    for (behavior, release, stage) in [
        ("early-release", b'x', "readiness"),
        ("missing-release", b'x', "release acknowledgement"),
        ("failed-exit", b'x', "process exit"),
        ("normal", b'?', "release acknowledgement"),
    ] {
        let error = quarantine_case("owner", behavior, release)
            .await
            .unwrap_err();
        assert!(error.contains(stage), "{error}");
    }
}

/// A real connection has just entered draining when exclusive shutdown starts.
/// A deterministic gate models stalled teardown: both a cancelled wait and a
/// deadline leave the original future and lock owned. Releasing the gate allows
/// that same close to complete, after which the identity may be acquired again.
/// This injects a stalled lifecycle boundary, not an actual lost UDP close ACK.
#[tokio::test]
async fn outbound_endpoint_shutdown_wait_retains_lock_until_completion() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-shutdown-{:032x}",
        rand::random::<u128>()
    ));
    let policy = RuntimeIrohTransportPolicy {
        setup_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    let owner = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let identity = owner.endpoint_id();
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![b"mez-owner-test".to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let (lease, remote) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::join!(owner.connect(server.addr(), b"mez-owner-test"), async {
            server.accept().await.unwrap().await.unwrap()
        })
    })
    .await
    .unwrap();
    let lease = lease.unwrap();
    assert_eq!(remote.remote_id(), identity);
    drop(lease);
    let mut shutdown = owner.begin_shutdown().unwrap();
    let (release, gate) = tokio::sync::oneshot::channel();
    let original = shutdown.work.take().unwrap();
    shutdown.work = Some(Box::pin(async move {
        gate.await.unwrap();
        original.await
    }));
    {
        let mut wait = Box::pin(shutdown.finish());
        assert!(matches!(
            futures_util::poll!(&mut wait),
            std::task::Poll::Pending
        ));
        // Dropping this borrowed wait does not discard the owned close future.
    }
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    shutdown.resource.as_mut().unwrap().setup_timeout = Duration::from_millis(100);
    assert!(shutdown.finish().await.is_err());
    assert!(shutdown.resource.as_ref().unwrap().identity.is_some());
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    release.send(()).unwrap();
    shutdown.resource.as_mut().unwrap().setup_timeout = Duration::from_secs(10);
    shutdown.finish().await.unwrap();
    shutdown.finish().await.unwrap();
    assert!(shutdown.resource.is_none());
    assert!(shutdown.work.is_none());
    let restored = RemoteClientIdentity::load_or_create(&root).unwrap();
    assert_eq!(restored.endpoint_id(), identity);
    drop(shutdown);
    drop(restored);
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Two leases share one public endpoint identity but use independent QUIC
/// connections. Capacity is finite; closing a sibling leaves the other usable,
/// and a busy owner cannot shut down the endpoint or release its identity lock.
#[tokio::test]
async fn outbound_endpoint_leases_are_bounded_independent_and_identity_locked() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-owner-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let policy = RuntimeIrohTransportPolicy {
        max_connections: 2,
        setup_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    let owner = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let identity = owner.endpoint_id();
    assert_eq!(owner.clone().endpoint_id(), identity);
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    let server = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::generate())
        .alpns(vec![b"mez-owner-test".to_vec()])
        .relay_mode(iroh::RelayMode::Disabled)
        .clear_address_lookup()
        .bind()
        .await
        .unwrap();
    let accept = async {
        let first = server.accept().await.unwrap().await.unwrap();
        let second = server.accept().await.unwrap().await.unwrap();
        (first, second)
    };
    let connect = async {
        let first = owner
            .connect(server.addr(), b"mez-owner-test")
            .await
            .unwrap();
        let second = owner
            .connect(server.addr(), b"mez-owner-test")
            .await
            .unwrap();
        (first, second)
    };
    let ((first, second), (remote_first, remote_second)) =
        tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(connect, accept)
        })
        .await
        .unwrap();
    assert_eq!(remote_first.remote_id(), identity);
    assert_eq!(remote_second.remote_id(), identity);
    assert_ne!(
        first.connection().stable_id(),
        second.connection().stable_id()
    );
    let full = owner
        .connect(server.addr(), b"mez-owner-test")
        .await
        .err()
        .unwrap();
    assert_eq!(full.kind(), MezErrorKind::RateLimited);
    assert!(owner.clone().begin_shutdown().is_err());
    drop(first);
    let send = async {
        let (mut send, mut recv) = second.connection().open_bi().await.unwrap();
        send.write_all(b"sibling").await.unwrap();
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(32).await.unwrap(), b"alive");
    };
    let reply = async {
        let (mut send, mut recv) = remote_second.accept_bi().await.unwrap();
        assert_eq!(recv.read_to_end(32).await.unwrap(), b"sibling");
        send.write_all(b"alive").await.unwrap();
        send.finish().unwrap();
    };
    tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(send, reply) })
        .await
        .unwrap();
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    drop(second);
    owner.begin_shutdown().unwrap().finish().await.unwrap();
    let restored = RemoteClientIdentity::load_or_create(&root).unwrap();
    assert_eq!(restored.endpoint_id(), identity);
    drop(restored);
    server.close().await;
    std::fs::remove_dir_all(root).unwrap();
}

/// Disabled policy rejects before protected key creation, so an explicit
/// outbound veto cannot be bypassed by the transport-resource constructor.
#[tokio::test]
async fn outbound_endpoint_disabled_policy_performs_no_key_io() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-disabled-{:032x}",
        rand::random::<u128>()
    ));
    let policy = RuntimeIrohTransportPolicy {
        outbound_enabled: false,
        ..Default::default()
    };
    assert!(OutboundEndpointOwner::bind(&root, &policy).await.is_err());
    assert!(!root.exists());
}

/// A polled handshake owns a capacity slot, but cancelling its future releases
/// that slot without starting detached application work or releasing the key
/// lock. An owner may be dropped while a sibling clone retains the endpoint.
#[tokio::test]
async fn outbound_endpoint_cancelled_attempt_releases_only_its_slot() {
    let root = std::env::temp_dir().join(format!(
        "mez-outbound-cancel-{:032x}",
        rand::random::<u128>()
    ));
    let policy = RuntimeIrohTransportPolicy {
        max_connections: 1,
        setup_timeout: Duration::from_secs(10),
        ..Default::default()
    };
    let owner = OutboundEndpointOwner::bind(&root, &policy).await.unwrap();
    let sibling = owner.clone();
    let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = iroh::EndpointAddr::new(iroh::SecretKey::generate().public())
        .with_ip_addr(blackhole.local_addr().unwrap());
    let mut attempt = Box::pin(owner.connect(target.clone(), b"mez-owner-test"));
    assert!(matches!(
        futures_util::poll!(&mut attempt),
        std::task::Poll::Pending
    ));
    assert_eq!(owner.inner.slots.available_permits(), 0);
    let full = sibling
        .connect(target, b"mez-owner-test")
        .await
        .err()
        .unwrap();
    assert_eq!(full.kind(), MezErrorKind::RateLimited);
    drop(attempt);
    assert_eq!(owner.inner.slots.available_permits(), 1);
    drop(owner);
    assert!(RemoteClientIdentity::load_or_create(&root).is_err());
    sibling.begin_shutdown().unwrap().finish().await.unwrap();
    drop(RemoteClientIdentity::load_or_create(&root).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

/// Direct callers cannot bypass configured resource bounds with an oversized
/// semaphore or deadline. Invalid budgets fail before any protected key I/O.
#[tokio::test]
async fn outbound_endpoint_invalid_budgets_perform_no_key_io() {
    for (connections, deadline) in [
        (0, Duration::from_secs(10)),
        (1025, Duration::from_secs(10)),
        (1, Duration::ZERO),
        (1, Duration::from_secs(121)),
    ] {
        let root = std::env::temp_dir().join(format!(
            "mez-outbound-budget-{:032x}",
            rand::random::<u128>()
        ));
        let policy = RuntimeIrohTransportPolicy {
            max_connections: connections,
            setup_timeout: deadline,
            ..Default::default()
        };
        assert!(OutboundEndpointOwner::bind(&root, &policy).await.is_err());
        assert!(!root.exists());
    }
}
