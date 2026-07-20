//! Linux-only adversarial tests for the real Bubblewrap execution boundary.
//!
//! These tests compile production launch plans and execute them through the
//! typed pane-shell transaction renderer. Hosts without a usable Bubblewrap
//! user-namespace profile report an explicit skip instead of conflating host
//! support with a product probe or policy failure.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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
        let workspace = root.join("workspace");
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
        PathScopes::try_shell_resolved_with_evidence(
            self.workspace.to_string_lossy(),
            vec![self.workspace.to_string_lossy().into_owned()],
            vec![self.target.to_string_lossy().into_owned()],
            evidence,
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
            "real-linux-pane-environment",
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
    let launch = ShellChildLaunch::new(plan.executable, arguments).unwrap();
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
    compile_bubblewrap_launch_plan(BubblewrapCompileRequest {
        config,
        capability,
        pane_environment_signature: "real-linux-pane-environment",
        network_policy: NetworkPolicy::Prompt,
        maximum_authority: authority,
        permission_evaluation: evaluation,
        child_shell_path: "/bin/sh",
        command_file_host_path: BUBBLEWRAP_COMMAND_FILE_HOST_PLACEHOLDER,
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
