//! Linux-only adversarial tests for the real Bubblewrap execution boundary.
//!
//! These tests compile production launch plans and execute them through the
//! typed pane-shell transaction renderer. Hosts without a usable Bubblewrap
//! user-namespace profile report an explicit skip instead of conflating host
//! support with a product probe or policy failure.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::symlink;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mez_agent::permissions::{
    EffectCompleteness, PathScopes, ResolvedPathEvidence, ResolvedPathKind,
};
use mez_agent::{
    MarkerToken, ShellChildArgument, ShellChildLaunch, ShellClassification, ShellTransaction,
};

use super::*;

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

/// Filesystem and host-service fixtures used by one real sandbox launch.
struct RealBubblewrapFixture {
    root: PathBuf,
    workspace: PathBuf,
    source: PathBuf,
    target: PathBuf,
    sibling: PathBuf,
    host_home: PathBuf,
    host_socket: PathBuf,
}

impl RealBubblewrapFixture {
    /// Creates disjoint visible, writable, sibling, and host-home trees.
    fn new(label: &str) -> Self {
        let unique = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "mez-real-bubblewrap-{label}-{}-{nanos}-{unique}",
                std::process::id()
            ));
        let workspace = root.join("home").join("alice");
        let source = workspace.join("src");
        let target = workspace.join("target");
        let sibling = root.join("sibling");
        let host_home = root.join("host-home");
        let host_socket = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("mez-bwrap-{unique}.sock"));
        for path in [&source, &target, &sibling, &host_home] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(source.join("visible.txt"), "visible\n").unwrap();
        fs::write(workspace.join("root-only.txt"), "root-only\n").unwrap();
        for protected in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".azure",
            ".kube",
            ".docker",
            ".config/mezzanine",
        ] {
            fs::create_dir_all(workspace.join(protected)).unwrap();
        }
        fs::write(workspace.join(".ssh/id_test"), "credential-secret\n").unwrap();
        fs::write(sibling.join("secret.txt"), "sibling-secret\n").unwrap();
        fs::write(host_home.join("secret.txt"), "home-secret\n").unwrap();
        symlink(sibling.join("secret.txt"), workspace.join("escape-link")).unwrap();
        Self {
            root,
            workspace,
            source,
            target,
            sibling,
            host_home,
            host_socket,
        }
    }

    /// Builds pane-resolved maximum authority for the fixture workspace.
    fn authority(&self) -> PathScopes {
        let mut evidence = BTreeMap::new();
        for (requested, canonical) in [
            (".", self.workspace.as_path()),
            ("src", self.source.as_path()),
            ("target", self.target.as_path()),
        ] {
            evidence.insert(
                requested.to_string(),
                ResolvedPathEvidence {
                    canonical_path: canonical.to_string_lossy().into_owned(),
                    kind: ResolvedPathKind::Existing,
                    nearest_existing_parent: canonical.to_string_lossy().into_owned(),
                },
            );
        }
        for protected in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".azure",
            ".kube",
            ".docker",
            ".config/mezzanine",
        ] {
            let canonical = self.workspace.join(protected);
            evidence.insert(
                canonical.to_string_lossy().into_owned(),
                ResolvedPathEvidence {
                    canonical_path: canonical.to_string_lossy().into_owned(),
                    kind: ResolvedPathKind::Existing,
                    nearest_existing_parent: canonical.to_string_lossy().into_owned(),
                },
            );
        }
        PathScopes::try_shell_resolved_with_evidence(
            self.workspace.to_string_lossy(),
            vec![self.workspace.to_string_lossy().into_owned()],
            vec![self.target.to_string_lossy().into_owned()],
            evidence,
        )
        .unwrap()
    }

    /// Extends the fixture's pane-resolved maximum read authority with
    /// canonical roots explicitly authenticated by the owning test.
    fn authority_with_additional_reads(&self, additional_reads: &[&Path]) -> PathScopes {
        let authority = self.authority();
        let mut read_scopes = authority.read_scopes;
        read_scopes.extend(
            additional_reads
                .iter()
                .map(|path| path.to_string_lossy().into_owned()),
        );
        PathScopes::try_shell_resolved_with_evidence(
            authority.current_directory,
            read_scopes,
            authority.write_scopes,
            authority.path_evidence,
        )
        .unwrap()
    }
}

impl Drop for RealBubblewrapFixture {
    /// Removes all host-side fixture state after each launch.
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.host_socket);
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Returns a verified production-profile capability or explicitly skips when
/// the Linux host does not provide the required Bubblewrap facilities.
fn verified_capability(config: &BubblewrapConfig) -> Option<BubblewrapCapability> {
    if !Path::new(&config.executable).is_file() {
        eprintln!(
            "skipping real Bubblewrap test: {} is unavailable",
            config.executable
        );
        return None;
    }
    let plan = bubblewrap_capability_probe_plan(config, "/bin/sh").unwrap();
    let output = Command::new(&plan.executable)
        .args(&plan.arguments)
        .output()
        .unwrap();
    if !output.status.success() {
        eprintln!(
            "skipping real Bubblewrap test: production profile unsupported: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    Some(
        parse_bubblewrap_capability_probe(
            "%1",
            "real-linux-pane-environment",
            0,
            &plan,
            output.status.code().unwrap_or(1),
            &String::from_utf8_lossy(&output.stdout),
        )
        .unwrap(),
    )
}

/// Quotes one test-owned path for literal POSIX-shell use.
fn shell_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\"'\"'"))
}

/// Executes a production launch plan through the typed pane transaction seam.
fn execute_plan(plan: BubblewrapLaunchPlan, command: &str) -> Output {
    let arguments = plan
        .arguments
        .into_iter()
        .map(|argument| {
            if argument == BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER {
                ShellChildArgument::MaterializedCommandFile
            } else {
                ShellChildArgument::Literal(argument)
            }
        })
        .collect();
    let launch = ShellChildLaunch::new(plan.executable, arguments)
        .unwrap()
        .with_status_fd(BUBBLEWRAP_STATUS_FD)
        .unwrap();
    let transaction = ShellTransaction::new(
        MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap(),
        "real-bubblewrap-turn",
        "real-bubblewrap-agent",
        "%real-bubblewrap-pane",
        Path::new("/bin/sh"),
        command,
    )
    .unwrap()
    .with_child_launch(launch);
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let mut child = Command::new("/bin/sh")
        .env("MEZ_REAL_SANDBOX_SECRET", "must-not-leak")
        .env("SSH_AUTH_SOCK", "/run/user/1000/ssh-agent.sock")
        .env("GPG_AGENT_INFO", "/run/user/1000/gnupg/S.gpg-agent")
        .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus")
        .env("DOCKER_HOST", "unix:///run/user/1000/docker.sock")
        .env("DISPLAY", ":99")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.wrapper.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(50));
    stdin.write_all(input.payload.as_bytes()).unwrap();
    drop(stdin);
    child.wait_with_output().unwrap()
}

/// Compiles a real launch plan with the fixture's pane-resolved authority.
fn real_plan(
    config: &BubblewrapConfig,
    capability: BubblewrapCapability,
    authority: &PathScopes,
    evaluation: &PermissionEvaluation,
) -> BubblewrapLaunchPlan {
    let environment = identity::current_process_environment_signature().unwrap();
    compile_bubblewrap_launch_plan(BubblewrapCompileRequest {
        config,
        identity: resolve_sandbox_identity(&config.supplementary_groups, &environment).unwrap(),
        capability,
        pane_environment_signature: "real-linux-pane-environment",
        network_policy: NetworkPolicy::Prompt,
        maximum_authority: authority,
        permission_evaluation: evaluation,
        preserve_maximum_authority: false,
        child_shell_path: "/bin/sh",
        command_file_host_path: BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
        managed_home: None,
        pane_home_directory: None,
        toolchain_projection: None,
        stateful: false,
        interactive: false,
    })
    .unwrap()
}

#[test]
/// Proves the real kernel boundary permits configured reads and writes while
/// blocking sibling, symlink, read-only, inherited-environment, and network
/// access through the production compiler and typed transaction renderer.
fn real_bubblewrap_enforces_maximum_authority_and_isolation() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipping real Bubblewrap network test: /usr/bin/python3 is unavailable");
        return;
    }
    let fixture = RealBubblewrapFixture::new("maximum-authority");
    let tcp_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let tcp_port = tcp_listener.local_addr().unwrap().port();
    let _unix_listener = UnixListener::bind(&fixture.host_socket).unwrap();
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);
    let command = format!(
        "set -eu\n\
         test \"$(cat src/visible.txt)\" = visible\n\
         printf '%s\\n' written > target/written.txt\n\
         if printf '%s\\n' forbidden > src/visible.txt 2>/dev/null; then exit 21; fi\n\
         test ! -r escape-link\n\
         test ! -e {}\n\
         test ! -S {}\n\
         test ! -e /etc/passwd\n\
         test -r /proc/self/status\n\
         test -c /dev/null\n\
         test \"$HOME\" = /home/mez\n\
         test \"$TMPDIR\" = /tmp\n\
         test -z \"${{MEZ_REAL_SANDBOX_SECRET+x}}\"\n\
         printf synthetic-home > \"$HOME/inside.txt\"\n\
         printf private-tmp > \"$TMPDIR/inside.txt\"\n\
         /usr/bin/python3 -c 'import socket,sys; s=socket.socket(); s.settimeout(0.2); sys.exit(0 if s.connect_ex((\"127.0.0.1\", {})) != 0 else 1)'\n\
         printf '%s\\n' REAL_BWRAP_MAXIMUM_OK",
        shell_quote(&fixture.sibling.join("secret.txt")),
        shell_quote(&fixture.host_socket),
        tcp_port,
    );

    let output = execute_plan(plan, &command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("REAL_BWRAP_MAXIMUM_OK"),
        "status={:?} stdout={stdout:?} stderr={stderr:?}",
        output.status
    );
    assert_eq!(
        fs::read_to_string(fixture.target.join("written.txt")).unwrap(),
        "written\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.source.join("visible.txt")).unwrap(),
        "visible\n"
    );
    assert!(!fixture.host_home.join("inside.txt").exists());
}

/// Proves a configured exact Unix socket beneath the protected user-runtime
/// root is projected read-only and remains connectable when the command has
/// separately authorized host networking.
#[test]
fn real_bubblewrap_projects_configured_ipc_socket() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipping real Bubblewrap IPC socket test: /usr/bin/python3 is unavailable");
        return;
    }
    let socket_root = Path::new("/run/user").join(format!(
        "mez-ipc-socket-{}-{}",
        std::process::id(),
        NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&socket_root);
    if let Err(error) = fs::create_dir_all(&socket_root) {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!(
                "skipping real Bubblewrap IPC socket test: cannot create {}",
                socket_root.display()
            );
            return;
        }
        panic!(
            "create IPC socket fixture {}: {error}",
            socket_root.display()
        );
    }
    let socket_path = socket_root.join("service.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let (received_sender, received) = mpsc::channel();
    let receiver = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut message = String::new();
        stream.read_to_string(&mut message).unwrap();
        received_sender.send(message).unwrap();
    });
    let fixture = RealBubblewrapFixture::new("configured-ipc-socket");
    let mut authorized = effects();
    authorized.network = true;
    authorized.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, authorized);
    let authority = fixture.authority_with_additional_reads(&[socket_path.as_path()]);
    let plan = real_plan(&config, capability, &authority, &evaluation);
    let command = format!(
        "test -S {socket} && printf socket-message | /usr/bin/python3 -c 'import socket,sys; connection=socket.socket(socket.AF_UNIX); connection.connect(sys.argv[1]); connection.sendall(sys.stdin.buffer.read()); connection.close()' {socket} && printf '%s\\n' REAL_BWRAP_IPC_SOCKET_OK",
        socket = shell_quote(&socket_path),
    );

    let output = execute_plan(plan, &command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_IPC_SOCKET_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        "socket-message"
    );
    receiver.join().unwrap();
    fs::remove_file(&socket_path).unwrap();
    fs::remove_dir_all(&socket_root).unwrap();
}

#[test]
/// Proves an advisory outside-path operand does not prevent compilation or
/// payload launch. The command starts inside Bubblewrap and then returns a
/// normal nonzero workload status because the host path is not projected.
fn real_bubblewrap_advisory_outside_path_fails_inside_payload() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    let fixture = RealBubblewrapFixture::new("advisory-outside-path");
    let outside_path = fixture.host_home.join("secret.txt");
    let mut advisory = effects();
    advisory.reads = vec![outside_path.to_string_lossy().into_owned()];
    let evaluation = evaluation(EffectCompleteness::Unknown, advisory);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);
    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Maximum
    );
    assert!(
        !plan
            .arguments
            .iter()
            .any(|argument| argument == &outside_path.to_string_lossy())
    );

    let command = format!(
        "printf '%s\\n' REAL_BWRAP_ADVISORY_PAYLOAD_STARTED\ncat {}",
        shell_quote(&outside_path),
    );
    let output = execute_plan(plan, &command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_ADVISORY_PAYLOAD_STARTED"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains(";D;1;mez_marker="), "stdout={stdout:?}");
}

#[test]
/// Proves an authorized network effect selects the connected Bubblewrap
/// profile while retaining the production filesystem confinement plan.
fn real_bubblewrap_authorized_network_uses_connected_profile() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!(
            "skipping real Bubblewrap connected-network test: /usr/bin/python3 is unavailable"
        );
        return;
    }
    let fixture = RealBubblewrapFixture::new("connected-network");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut network = effects();
    network.network = true;
    let evaluation = evaluation(EffectCompleteness::Complete, network);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);
    assert_eq!(plan.audit_summary.network, BubblewrapNetworkMode::Connected);
    assert!(!plan.arguments.contains(&"--unshare-net".to_string()));

    let command = format!(
        "/usr/bin/python3 -c 'import socket; s=socket.create_connection((\"127.0.0.1\", {}), 1); s.close()' && printf '%s\\n' REAL_BWRAP_CONNECTED_OK",
        port,
    );
    let output = execute_plan(plan, &command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_CONNECTED_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// Proves the production Bubblewrap launch can execute the selected host Rust
/// toolchain while Cargo credentials and configuration remain outside the
/// projected filesystem.
fn real_bubblewrap_projects_read_only_rust_toolchain() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("skipping real Bubblewrap Rust test: HOME is unavailable");
        return;
    };
    let cargo_bin = home.join(".cargo/bin");
    let rustup_home = home.join(".rustup");
    if !cargo_bin.join("cargo").exists() || !rustup_home.is_dir() {
        eprintln!("skipping real Bubblewrap Rust test: rustup-managed Cargo is unavailable");
        return;
    }
    let Ok(cargo_bin) = cargo_bin.canonicalize() else {
        eprintln!("skipping real Bubblewrap Rust test: Cargo bin cannot be canonicalized");
        return;
    };
    let Ok(rustup_home) = rustup_home.canonicalize() else {
        eprintln!("skipping real Bubblewrap Rust test: Rustup home cannot be canonicalized");
        return;
    };
    let mut config = config();
    config.toolchains = vec![SandboxToolchainKind::Rust];
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    let fixture = RealBubblewrapFixture::new("rust-toolchain");
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);
    let managers = [
        format!("cargo-bin:{}", cargo_bin.display()),
        format!("rustup:{}", rustup_home.display()),
    ];
    let projection = resolve_toolchain_projection(&config.toolchains, &managers, "linux")
        .unwrap()
        .unwrap();
    let authority =
        fixture.authority_with_additional_reads(&[cargo_bin.as_path(), rustup_home.as_path()]);
    let environment = identity::current_process_environment_signature().unwrap();
    let plan = compile_bubblewrap_launch_plan(BubblewrapCompileRequest {
        config: &config,
        identity: resolve_sandbox_identity(&config.supplementary_groups, &environment).unwrap(),
        capability,
        pane_environment_signature: "real-linux-pane-environment",
        network_policy: NetworkPolicy::Prompt,
        maximum_authority: &authority,
        permission_evaluation: &evaluation,
        preserve_maximum_authority: false,
        child_shell_path: "/bin/sh",
        command_file_host_path: BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
        managed_home: None,
        pane_home_directory: None,
        toolchain_projection: Some(&projection),
        stateful: false,
        interactive: false,
    })
    .unwrap();
    let output = execute_plan(
        plan,
        "set -eu\n\
         cargo --version\n\
         rustc --version\n\
         test \"$CARGO_HOME\" = /home/mez/.cargo\n\
         test \"$RUSTUP_HOME\" = /opt/mez/toolchains/rust/rustup\n\
         test ! -e \"$CARGO_HOME/credentials.toml\"\n\
         test ! -e \"$CARGO_HOME/config.toml\"\n\
         printf '%s\\n' REAL_BWRAP_RUST_TOOLCHAIN_OK",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_RUST_TOOLCHAIN_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// Proves a real sandboxed Git commit uses only the configured identity pair,
/// overriding repository-local identity without importing host-global Git
/// configuration or any credential/signing settings.
fn real_bubblewrap_projects_sanitized_git_identity() {
    let mut config = config();
    config.git_user_name = Some("Sandbox Author".to_string());
    config.git_user_email = Some("sandbox@example.invalid".to_string());
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    if !Path::new("/usr/bin/git").is_file() {
        eprintln!("skipping real Bubblewrap Git identity test: /usr/bin/git is unavailable");
        return;
    }
    let fixture = RealBubblewrapFixture::new("git-identity");
    fs::write(
        fixture.host_home.join(".gitconfig"),
        "[credential]\n\thelper = host-secret-helper\n[user]\n\tname = Host Author\n",
    )
    .unwrap();
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);
    let output = execute_plan(
        plan,
        "set -eu\n\
         mkdir -p target/repo\n\
         cd target/repo\n\
         git init -q\n\
         git config user.name 'Repository Author'\n\
         git config user.email 'repository@example.invalid'\n\
         printf tracked > tracked.txt\n\
         git add tracked.txt\n\
         git commit -q -m identity\n\
         test \"$(git log -1 --format=%an)\" = 'Sandbox Author'\n\
         test \"$(git log -1 --format=%ae)\" = 'sandbox@example.invalid'\n\
         test \"$(git config user.name)\" = 'Sandbox Author'\n\
         test -z \"$(git config --global --get credential.helper || true)\"\n\
         test -z \"$(git config --global --get user.signingkey || true)\"\n\
         printf '%s\\n' REAL_BWRAP_GIT_IDENTITY_OK",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_GIT_IDENTITY_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// Proves a broad host-backed authority exposes only its configured scope
/// without implicit credential-descendant masking.
fn real_bubblewrap_does_not_mask_configured_credential_descendants() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    let fixture = RealBubblewrapFixture::new("configured-credentials");
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);

    let output = execute_plan(
        plan,
        "set -eu\n\
         test \"$(cat root-only.txt)\" = root-only\n\
         test \"$(cat .ssh/id_test)\" = credential-secret\n\
         printf '%s\\n' REAL_BWRAP_CONFIGURED_SCOPE_OK",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_CONFIGURED_SCOPE_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.workspace.join(".ssh/id_test")).unwrap(),
        "credential-secret\n"
    );
}

#[test]
/// Proves complete effects produce a real narrowed mount graph: selected
/// source and target paths remain usable while an otherwise-authorized
/// workspace-root file is absent from the sandbox.
fn real_bubblewrap_complete_effects_narrow_visible_mounts() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    let fixture = RealBubblewrapFixture::new("narrowed-authority");
    let mut complete = effects();
    complete.reads.push("src".to_string());
    complete.writes.push("target".to_string());
    let evaluation = evaluation(EffectCompleteness::Complete, complete);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);
    assert_eq!(
        plan.audit_summary.authority_source,
        SandboxAuthoritySource::Narrowed
    );
    let output = execute_plan(
        plan,
        "set -eu\n\
         test \"$(cat src/visible.txt)\" = visible\n\
         test ! -e root-only.txt\n\
         test ! -e escape-link\n\
         printf narrowed > target/narrowed.txt\n\
         printf '%s\\n' REAL_BWRAP_NARROWED_OK",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_NARROWED_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(fixture.target.join("narrowed.txt")).unwrap(),
        "narrowed"
    );
}

#[test]
/// Proves a failed typed sandbox executable never retries the materialized
/// workload as an ordinary policy-only shell command.
fn typed_launch_failure_has_no_unsandboxed_fallback_side_effect() {
    let fixture = RealBubblewrapFixture::new("no-fallback");
    let side_effect = fixture.target.join("must-not-exist.txt");
    let launch = ShellChildLaunch::new(
        "/definitely/missing/mez-bwrap",
        vec![ShellChildArgument::MaterializedCommandFile],
    )
    .unwrap();
    let transaction = ShellTransaction::new(
        MarkerToken::new("fedcba9876543210fedcba9876543210").unwrap(),
        "failed-bubblewrap-turn",
        "failed-bubblewrap-agent",
        "%failed-bubblewrap-pane",
        Path::new("/bin/sh"),
        format!("printf fallback > {}", shell_quote(&side_effect)),
    )
    .unwrap()
    .with_child_launch(launch);
    let input = transaction.render_for_classification_input(ShellClassification::PosixSh);
    let mut child = Command::new("/bin/sh")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input.wrapper.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(50));
    stdin.write_all(input.payload.as_bytes()).unwrap();
    drop(stdin);
    let output = child.wait_with_output().unwrap();

    assert!(
        !side_effect.exists(),
        "sandbox launch failure ran the workload"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(";D;127;mez_marker="),
        "stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
/// Proves the private network and PID namespaces block IPv6 loopback, DNS,
/// Linux abstract Unix sockets, and visibility of a known host process while
/// the minimal environment removes representative credential and IPC handles.
fn real_bubblewrap_blocks_extended_host_network_ipc_and_process_access() {
    let config = config();
    let Some(capability) = verified_capability(&config) else {
        return;
    };
    if !Path::new("/usr/bin/python3").is_file() {
        eprintln!("skipping extended real Bubblewrap test: /usr/bin/python3 is unavailable");
        return;
    }
    let ipv6_listener = match TcpListener::bind("[::1]:0") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipping extended real Bubblewrap test: IPv6 loopback unavailable: {error}");
            return;
        }
    };
    let ipv6_port = ipv6_listener.local_addr().unwrap().port();
    let abstract_name = format!(
        "mez-bwrap-{}-{}",
        std::process::id(),
        NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    );
    let abstract_address = SocketAddr::from_abstract_name(abstract_name.as_bytes()).unwrap();
    let _abstract_listener = UnixListener::bind_addr(&abstract_address).unwrap();
    let host_pid = std::process::id();
    let fixture = RealBubblewrapFixture::new("extended-isolation");
    let mut unknown = effects();
    unknown.unknown = true;
    let evaluation = evaluation(EffectCompleteness::Unknown, unknown);
    let plan = real_plan(&config, capability, &fixture.authority(), &evaluation);
    let command = format!(
        "set -eu\n\
         test -z \"${{SSH_AUTH_SOCK+x}}\"\n\
         test -z \"${{GPG_AGENT_INFO+x}}\"\n\
         test -z \"${{DBUS_SESSION_BUS_ADDRESS+x}}\"\n\
         test -z \"${{DOCKER_HOST+x}}\"\n\
         test -z \"${{DISPLAY+x}}\"\n\
         test ! -e /proc/{}\n\
         /usr/bin/python3 -c 'import socket,sys; s=socket.socket(socket.AF_INET6); s.settimeout(0.2); sys.exit(0 if s.connect_ex((\"::1\", {})) != 0 else 1)'\n\
         if /usr/bin/python3 -c 'import socket; socket.getaddrinfo(\"example.com\", 80)' 2>/dev/null; then exit 22; fi\n\
         /usr/bin/python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.settimeout(0.2); sys.exit(0 if s.connect_ex(\"\\0{}\") != 0 else 1)'\n\
         printf '%s\\n' REAL_BWRAP_EXTENDED_OK",
        host_pid, ipv6_port, abstract_name,
    );

    let output = execute_plan(plan, &command);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("REAL_BWRAP_EXTENDED_OK"),
        "status={:?} stdout={stdout:?} stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}
