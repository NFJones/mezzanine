//! Private normal-RC environment and failure cleanup for managed Fish admission.
//!
//! Only this test-owned single manager root is covered. The existing manager's
//! successful native wait receipt, not map absence or Drop, permits directory
//! removal. Startup/helper/assertion panics are resumed after separate cleanup;
//! an unresolved native or filesystem outcome retains its owned evidence path.

use super::*;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// Owns one exclusively created environment; intentionally has no deleting Drop.
pub(super) struct IsolatedFishFixture {
    root: PathBuf,
    environment: Vec<(String, String)>,
}

impl IsolatedFishFixture {
    /// Creates a private root without predeleting or adopting an existing path.
    pub(super) fn new(fish: &Path) -> std::io::Result<Self> {
        let candidate = std::env::temp_dir().join(format!(
            "mez-fish-admission-{:032x}",
            rand::random::<u128>()
        ));
        create_private_directory(&candidate)?;
        let root = candidate.canonicalize()?;
        match Self::build(&root, fish) {
            Ok(environment) => Ok(Self { root, environment }),
            Err(error) => {
                // The root is exclusively owned by this fixture, so a failed
                // construction must not leave it behind unreported.
                let _ = std::fs::remove_dir_all(&root);
                Err(std::io::Error::other(format!(
                    "fish admission fixture setup failed for {}: {error}",
                    root.display()
                )))
            }
        }
    }

    /// Builds the owned launch environment inside an existing private root.
    fn build(root: &Path, fish: &Path) -> std::io::Result<Vec<(String, String)>> {
        let mut environment = Vec::new();
        for (key, directory) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_RUNTIME_DIR", "runtime"),
            ("TMPDIR", "tmp"),
        ] {
            let path = root.join(directory);
            create_private_directory(&path)?;
            environment.push((key.to_string(), utf8_path(&path)?));
        }
        // The launch context replaces the ambient environment. These locations
        // supply the selected Fish and the adapter's portable POSIX utilities,
        // including sha256sum (Linux) or shasum (macOS). MEZ/TERM/SHELL remain
        // injected by the real mux launch owner, not synthetic protocol values.
        let fish_parent = fish
            .parent()
            .ok_or_else(|| std::io::Error::other("Fish has no parent directory"))?;
        let path = std::env::join_paths([
            fish_parent,
            Path::new("/usr/bin"),
            Path::new("/bin"),
            Path::new("/usr/sbin"),
            Path::new("/sbin"),
            Path::new("/usr/local/bin"),
            Path::new("/opt/homebrew/bin"),
        ])
        .map_err(std::io::Error::other)?;
        environment.push((
            "PATH".into(),
            path.into_string()
                .map_err(|_| std::io::Error::other("fixture PATH is not UTF-8"))?,
        ));
        environment.push(("LC_ALL".into(), "C".into()));
        let fish_config = root.join("config/fish");
        create_private_directory(&fish_config)?;
        let mut config = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(fish_config.join("config.fish"))?;
        config.write_all(b"set -g fish_greeting ''\nset -l __mez_fixture_adapter absent\nif functions -q __mez_agent_wrapper_receive\n    set __mez_fixture_adapter present\nend\nbuiltin printf '%s %s\\n' $fish_pid $__mez_fixture_adapter >> \"$HOME/normal-rc-witness\"\nfunction fish_prompt\n    builtin printf '__MEZ_TEST_NORMAL_RC__> '\nend\nfunction fish_right_prompt\nend\n")?;
        Ok(environment)
    }

    /// Returns the owned physical root retained on uncertain cleanup.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Returns only explicit fixture variables; no ambient user values are read.
    pub(super) fn environment(&self) -> &[(String, String)] {
        &self.environment
    }

    /// Builds the original legacy/pane/policy-only fixture without spawning.
    pub(super) fn service(&self, fish: PathBuf) -> crate::error::Result<RuntimeSessionService> {
        let mut service = RuntimeSessionService::with_event_log(
            Session::new_default(
                ResolvedShell::new(fish, ShellSource::ShellEnv),
                Size::new(80, 24)?,
            )?,
            self.root.join("default.sock"),
            100,
            10,
            1024,
        )?;
        configure_pane_shell_protocol_fixture(&mut service);
        *service.host_clipboard_mut_for_tests() = HostClipboard::disabled();
        Ok(service)
    }

    /// Checks this native parent's normal RC executed before adapter definition.
    /// Nested Fish may append its own record; no exactly-once RC claim is made.
    pub(super) fn assert_parent_rc_before_adapter(&self, parent: u32) {
        let witness = std::fs::read_to_string(self.root.join("home/normal-rc-witness")).unwrap();
        assert!(
            witness
                .lines()
                .any(|line| line == format!("{parent} absent")),
            "parent normal-RC ordering witness: {witness:?}"
        );
        assert!(
            !witness
                .lines()
                .any(|line| line == format!("{parent} present")),
            "adapter preceded parent normal RC: {witness:?}"
        );
    }

    /// Catches every potentially spawning operation in body, snapshots before
    /// cleanup, and calls the existing bounded manager owner exactly once.
    pub(super) fn run(
        &self,
        service: &mut RuntimeSessionService,
        body: impl FnOnce(&mut RuntimeSessionService),
    ) -> FishFixtureOutcome {
        let checked = catch_unwind(AssertUnwindSafe(|| body(service)));
        let primary_pid = service.pane_processes().primary_pid("%1");
        let snapshot = format!(
            "pid={primary_pid:?}; screen={:?}; readiness={:?}; authority={:?}; bootstrap={}; transactions={:?}; parent_return={}; discarded_draft_exists={}",
            service
                .process_pane_screen("%1")
                .map(|screen| screen.normal_content_lines().join("\n")),
            service.pane_readiness_state("%1"),
            service.pane_environment_authority("%1"),
            service.pane_bootstrap_is_pending_for_tests("%1"),
            service.running_shell_transactions_for_tests(),
            service.managed_shell_parent_restoration_is_pending_for_tests("%1"),
            self.root.join("discarded-fish-draft").exists()
        );
        let terminated = service.terminate_all_pane_processes();
        let (native_settled, removed) =
            remove_after_exact_receipt(&self.root, primary_pid, &terminated);
        let diagnostics = format!(
            "Fish fixture pre-cleanup: {snapshot}; termination={terminated:?}; native_settled={native_settled}; directory_cleanup={removed:?}; owned_root={}",
            self.root.display()
        );
        FishFixtureOutcome {
            checked,
            native_settled,
            removed,
            diagnostics,
        }
    }
}

/// Captured primary outcome plus independent native/filesystem cleanup evidence.
pub(super) struct FishFixtureOutcome {
    checked: std::thread::Result<()>,
    native_settled: bool,
    removed: std::result::Result<(), String>,
    diagnostics: String,
}

impl FishFixtureOutcome {
    /// Preserves the original panic payload even when cleanup also failed.
    pub(super) fn finish(self) {
        if self.checked.is_err() || !self.native_settled || self.removed.is_err() {
            eprintln!("{}", self.diagnostics);
        }
        if let Err(panic) = self.checked {
            resume_unwind(panic);
        }
        assert!(self.native_settled, "{}", self.diagnostics);
        assert!(self.removed.is_ok(), "{}", self.diagnostics);
    }
}

/// Creates one private directory atomically; existing candidates are untouched.
fn create_private_directory(path: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new().mode(0o700).create(path)
}

/// Rejects unrepresentable launch paths rather than changing their identity.
fn utf8_path(path: &Path) -> std::io::Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| std::io::Error::other("fixture path is not UTF-8"))
}

/// Directory removal requires a successful wait receipt for the sole captured
/// manager root. Absence, typed error or mismatched receipt conservatively keeps
/// evidence; this is not a claim that an absent root is necessarily still alive.
fn remove_after_exact_receipt(
    root: &Path,
    primary_pid: Option<u32>,
    terminated: &crate::error::Result<Vec<mez_mux::process::ExitedPaneProcess>>,
) -> (bool, std::result::Result<(), String>) {
    let settled = primary_pid.is_some_and(|pid| {
        terminated.as_ref().is_ok_and(|exits| {
        matches!(exits.as_slice(), [exit] if exit.pane_id == "%1" && exit.primary_pid == pid)
    })
    });
    if !settled {
        return (
            false,
            Err("native settlement unproven; owned directory retained".into()),
        );
    }
    (
        true,
        std::fs::remove_dir_all(root)
            .map_err(|error| format!("native root settled, directory removal failed: {error}")),
    )
}

/// A real isolated Fish root is reaped before the original assertion payload is
/// repropagated. Catching outside finish verifies identity rather than merely
/// inspecting an internal captured outcome, and no PID/group is signaled here.
#[test]
fn runtime_fish_fixture_panic_reaps_root_before_resuming_original_payload() {
    let Some(fish) = find_test_shell(
        "fish",
        &[
            "/usr/bin/fish",
            "/usr/local/bin/fish",
            "/opt/homebrew/bin/fish",
        ],
    ) else {
        return;
    };
    let fixture = IsolatedFishFixture::new(&fish).unwrap();
    let mut service = fixture.service(fish).unwrap();
    let identity = std::sync::Arc::new(());
    let sent = identity.clone();
    let outcome = fixture.run(&mut service, |service| {
        service
            .attach_primary("primary", true, Size::new(80, 24).unwrap(), 120)
            .unwrap();
        service
            .start_initial_pane_process_with_launch_context(
                None,
                Some(fixture.root()),
                Some(fixture.environment()),
            )
            .unwrap();
        std::panic::panic_any(sent);
    });
    assert!(outcome.native_settled, "{}", outcome.diagnostics);
    assert!(outcome.removed.is_ok(), "{}", outcome.diagnostics);
    assert!(!fixture.root().exists());
    let original = catch_unwind(AssertUnwindSafe(|| outcome.finish())).unwrap_err();
    let original = original
        .downcast::<std::sync::Arc<()>>()
        .expect("original panic type must survive cleanup");
    assert!(std::sync::Arc::ptr_eq(&original, &identity));
}

/// A previous evidence root must survive name collision and uncertain cleanup.
/// These negative policy controls spawn no native task and do not fabricate a
/// successful reap proof from an empty list, missing identity or typed error.
#[test]
fn runtime_fish_fixture_retains_existing_or_unproven_evidence_roots() {
    let fixture = IsolatedFishFixture::new(Path::new("/usr/bin/fish")).unwrap();
    let evidence = fixture.root().join("retained-evidence");
    std::fs::write(&evidence, b"original").unwrap();
    assert_eq!(
        create_private_directory(fixture.root()).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    for result in [
        Ok(Vec::new()),
        Err(crate::error::MezError::invalid_state(
            "synthetic cleanup failure",
        )),
    ] {
        let (settled, removed) = remove_after_exact_receipt(fixture.root(), None, &result);
        assert!(!settled);
        assert!(removed.is_err());
        assert_eq!(std::fs::read(&evidence).unwrap(), b"original");
    }
    // No native process was created in this pure negative-policy test.
    std::fs::remove_dir_all(fixture.root()).unwrap();
}
