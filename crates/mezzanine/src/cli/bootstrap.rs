//! Daemon-free bootstrap admission for compiled best-effort harness integrations.
//!
//! Candidate names are not certification. Versions do not gate compiled adapters.
//! This boundary never takes user-authored manifest
//! JSON or executable templates; adapters must enter the compiled registry.

use super::{Args, CliOutputFormat, MezError, PathBuf, Result, Write};

/// Default install/reconcile, with explicit read-only previews and maintenance.
#[derive(Debug, Clone, Args)]
pub(super) struct BootstrapCliArgs {
    /// Harness whose compiled adapter should be consulted.
    #[arg(value_parser = ["claude", "codex", "copilot", "opencode", "cursor", "pi"])]
    harness: String,
    /// Observed vendor version; best-effort adapters do not require a version pin.
    #[arg(long)]
    vendor_version: Option<String>,
    /// Optional user/project root override; otherwise use documented vendor user roots.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Preview install/uninstall/recovery without creating or changing any state.
    #[arg(long, conflicts_with = "check")]
    dry_run: bool,
    /// Check accepted ownership without publishing artifacts.
    #[arg(long, conflicts_with_all = ["uninstall", "recover"])]
    check: bool,
    /// Remove qualified owned integration entries, preserving shared siblings/archived edits.
    #[arg(long, conflicts_with = "recover")]
    uninstall: bool,
    /// Explicitly finish an already accepted publication journal.
    #[arg(long)]
    recover: bool,
}

/// Uses compiled best-effort artifacts and reports absent adapters honestly.
pub(super) fn run<W: Write>(
    args: BootstrapCliArgs,
    format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let manifest = crate::integrations::bootstrap::compiled_manifest(
        &args.harness,
        args.vendor_version.as_deref(),
    );
    run_with_manifest(args, format, stdout, manifest)
}

/// Executes only a compiled manifest matching the requested harness.
/// Tests inject content-free fixtures, not a process-visible manifest interface.
fn run_with_manifest<W: Write>(
    args: BootstrapCliArgs,
    format: CliOutputFormat,
    stdout: &mut W,
    manifest: Option<crate::integrations::bootstrap::installer::Manifest>,
) -> Result<()> {
    run_with_root_selector(
        args,
        format,
        stdout,
        manifest,
        crate::integrations::bootstrap::roots::resolve,
        process_home,
    )
}

/// Captures the standard private-state HOME without initializing user config.
/// Native state inspection validates its absolute path, ownership and witnesses.
fn process_home() -> Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        MezError::invalid_args("HOME is not set; cannot locate private bootstrap state")
    })
}

/// Root selection is injected only for isolated tests; publication still derives
/// authority from the compiled manifest and descriptor-safe actual destination.
fn run_with_root_selector<W: Write>(
    args: BootstrapCliArgs,
    format: CliOutputFormat,
    stdout: &mut W,
    manifest: Option<crate::integrations::bootstrap::installer::Manifest>,
    select_root: impl FnOnce(
        &str,
        Option<&std::path::Path>,
    ) -> Result<crate::integrations::bootstrap::roots::SelectedRoot>,
    select_home: impl FnOnce() -> Result<PathBuf>,
) -> Result<()> {
    if args.vendor_version.as_ref().is_some_and(|version| {
        version.is_empty() || version.len() > 128 || version.chars().any(char::is_control)
    }) {
        return Err(MezError::invalid_args(
            "bootstrap vendor version must be bounded inert text",
        ));
    }
    if let Some(manifest) = manifest {
        if manifest.harness != args.harness {
            return Err(MezError::invalid_args(
                "bootstrap manifest does not match the requested harness",
            ));
        }
        let selected = select_root(&args.harness, args.root.as_deref())?;
        let home = select_home()?;
        let root = &selected.path;
        use crate::integrations::bootstrap::installer::BootstrapOutcome;
        let mut recovered = false;
        let mut recovery_pending = false;
        let mut changed_paths = Vec::new();
        let mut planned_preserved_paths = Vec::new();
        let outcome;
        let operation = if args.recover {
            let report = if args.dry_run {
                crate::integrations::bootstrap::installer::preview_recovery_private(
                    root, &home, &manifest,
                )?
            } else {
                crate::integrations::bootstrap::installer::recover_private_report(
                    root, &home, &manifest,
                )?
            };
            outcome = if let Some(report) = report {
                changed_paths = report.changed_paths;
                planned_preserved_paths = report.preserved_paths;
                recovery_pending = args.dry_run;
                recovered = !args.dry_run;
                BootstrapOutcome::Recovered
            } else {
                BootstrapOutcome::Unchanged
            };
            "recover"
        } else {
            use crate::integrations::bootstrap::installer::{Operation, plan_private};
            let plan = plan_private(
                root,
                &home,
                &manifest,
                if args.uninstall {
                    Operation::Uninstall
                } else {
                    Operation::Install
                },
            )?;
            changed_paths = plan
                .changed_paths()
                .into_iter()
                .map(str::to_string)
                .collect();
            outcome = plan.outcome();
            planned_preserved_paths = plan
                .preserved_paths()
                .into_iter()
                .map(str::to_string)
                .collect();
            recovery_pending = plan.recovery_pending();
            if !args.dry_run && !args.check {
                plan.apply()?;
                recovered = recovery_pending;
                recovery_pending = false;
            }
            if args.uninstall {
                "uninstall"
            } else if args.check {
                "check"
            } else {
                "install"
            }
        };
        let result = if args.dry_run {
            "preview"
        } else if args.check {
            "checked"
        } else {
            outcome.as_str()
        };
        let preserved_paths = if args.dry_run || args.check {
            Vec::new()
        } else {
            planned_preserved_paths.clone()
        };
        return write_report(
            stdout,
            format,
            &serde_json::json!({
                "harness":args.harness,"vendor_version":args.vendor_version,"operation":operation,
                "scope_root":root.to_string_lossy(),"root_source":selected.source,
                "supported":true,"manifest_revision":manifest.revision,"changed_paths":changed_paths,"recovered":recovered,
                "dry_run":args.dry_run,"recovery_pending":recovery_pending,
                "result":result,"planned_outcome":outcome.as_str(),
                "planned_preserved_paths":planned_preserved_paths,"preserved_paths":preserved_paths,
                "runtime_verification":"not-performed",
                "support":"best-effort",
                "guidance":"Installation is observational only. Use ordinary vendor commands and preserve vendor review/disabled policy. Automatic enrollment and accounting capabilities may still be unavailable; no Mez vendor-launch wrappers exist",
            }),
        );
    }
    if !args.dry_run && !args.check {
        return Err(MezError::invalid_state(format!(
            "bootstrap {}: no compiled adapter manifest is installed; no files changed. Candidate support is not installation",
            args.harness
        )));
    }
    let output = serde_json::json!({
        "harness":args.harness, "vendor_version":args.vendor_version,
        "scope_root":args.root, "operation":if args.recover {"recover"} else if args.uninstall {"uninstall"} else if args.check {"check"} else {"install"},"dry_run":args.dry_run,
        "supported":false, "certification":"unavailable", "changed_paths":[],
        "result":"unavailable","planned_outcome":null,"planned_preserved_paths":[],"preserved_paths":[],"runtime_verification":"not-performed",
        "lifecycle":"unavailable", "usage":"unavailable",
        "reason":"No compiled adapter manifest; no vendor configuration, credentials or hook trust changed",
    });
    write_report(stdout, format, &output)
}

/// Renders compact operator diagnostics or the exact machine-readable report.
/// Paths use escaped debug spelling in plain text; no artifact/archive bytes or
/// vendor probing are involved. Missing adapter reports never imply activation.
fn write_report<W: Write>(
    stdout: &mut W,
    format: CliOutputFormat,
    report: &serde_json::Value,
) -> Result<()> {
    if format == CliOutputFormat::Json {
        writeln!(stdout, "{report}")?;
        return Ok(());
    }
    let result = report["result"].as_str().unwrap_or("unavailable");
    writeln!(
        stdout,
        "bootstrap {}: {result}",
        report["harness"].as_str().unwrap_or("unknown")
    )?;
    if let Some(planned) = report["planned_outcome"].as_str() {
        if matches!(result, "preview" | "checked") {
            writeln!(stdout, "Planned outcome: {planned} (no writes)")?;
        }
        writeln!(
            stdout,
            "Root: {:?} ({})",
            report["scope_root"].as_str().unwrap_or("not selected"),
            report["root_source"].as_str().unwrap_or("unknown")
        )?;
        writeln!(
            stdout,
            "Changes: {}; preservation: {} planned, {} confirmed; recovery: {}",
            report["changed_paths"].as_array().map_or(0, Vec::len),
            report["planned_preserved_paths"]
                .as_array()
                .map_or(0, Vec::len),
            report["preserved_paths"].as_array().map_or(0, Vec::len),
            if report["recovered"] == true {
                "completed"
            } else if report["recovery_pending"] == true {
                "pending"
            } else {
                "none"
            },
        )?;
        writeln!(
            stdout,
            "Runtime enrollment/accounting not verified; vendor review and disabled policy remain unchanged."
        )?;
    } else {
        writeln!(stdout, "No compiled adapter; no files changed.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Owns a unique physical private HOME, separate from vendor fixture roots.
    /// No unit fixture changes process HOME or writes ambient user configuration.
    struct PrivateHome(PathBuf);

    impl PrivateHome {
        /// Creates only a new test-owned HOME with safe initial Unix permissions.
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            let path = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!(
                    "mez-bootstrap-home-{}",
                    crate::storage::token_usage::new_token_usage_event_id()
                ));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
            Self(path)
        }
    }

    impl Drop for PrivateHome {
        /// Best-effort fixture-only cleanup avoids double panic after failures.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Drives the compiled production CLI with explicit isolated private HOME.
    /// Explicit vendor roots still use the real literal root-selector policy.
    fn run_at_home<W: Write>(
        args: BootstrapCliArgs,
        format: CliOutputFormat,
        stdout: &mut W,
        home: &std::path::Path,
    ) -> Result<()> {
        let manifest = crate::integrations::bootstrap::compiled_manifest(
            &args.harness,
            args.vendor_version.as_deref(),
        );
        run_manifest_at_home(args, format, stdout, manifest, home)
    }

    /// Injects fixture-only compiled artifacts and HOME through the same CLI
    /// owner; no public manifest or state-directory override is introduced.
    fn run_manifest_at_home<W: Write>(
        args: BootstrapCliArgs,
        format: CliOutputFormat,
        stdout: &mut W,
        manifest: Option<crate::integrations::bootstrap::installer::Manifest>,
        home: &std::path::Path,
    ) -> Result<()> {
        run_with_root_selector(
            args,
            format,
            stdout,
            manifest,
            crate::integrations::bootstrap::roots::resolve,
            || Ok(home.to_path_buf()),
        )
    }

    /// Captures isolated fixture bytes and Unix modes without following links.
    /// Complete HOME snapshots qualify both vendor and private-state nonmutation.
    fn tree_snapshot(root: &std::path::Path) -> Vec<(PathBuf, u32, Option<Vec<u8>>)> {
        use std::os::unix::fs::PermissionsExt;
        fn visit(
            root: &std::path::Path,
            path: &std::path::Path,
            entries: &mut Vec<(PathBuf, u32, Option<Vec<u8>>)>,
        ) {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            entries.push((
                path.strip_prefix(root).unwrap().into(),
                metadata.permissions().mode(),
                metadata.is_file().then(|| std::fs::read(path).unwrap()),
            ));
            if metadata.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), entries);
                }
            }
        }
        let mut entries = Vec::new();
        visit(root, root, &mut entries);
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }

    /// Parses real CLI argument bytes and runs the compiled adapter at isolated
    /// HOME. Helpers supply only test paths, never alternate artifact authority.
    fn invoke_private_fixture(
        home: &std::path::Path,
        root: &std::path::Path,
        flags: &[&str],
    ) -> Result<serde_json::Value> {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let mut args = vec![
            std::ffi::OsString::from("fixture"),
            "pi".into(),
            "--root".into(),
            root.as_os_str().to_owned(),
        ];
        args.extend(flags.iter().map(std::ffi::OsString::from));
        let parsed = Fixture::try_parse_from(args).unwrap();
        let mut output = Vec::new();
        run_at_home(parsed.args, CliOutputFormat::Json, &mut output, home)?;
        Ok(serde_json::from_slice(&output).unwrap())
    }

    /// CLI result labels derive from accepted ownership and actual publication,
    /// not exit success or runtime activation. Read-only previews/checks preserve
    /// both trees; repairs report archived destinations without exposing bytes.
    /// Historical upgrade and pending recovery use the same production owners.
    /// Public Codex bootstrap merges shared authored callbacks through the same
    /// private owner as its previews. Historical ownership upgrade can change only
    /// the receipt while retaining byte-exact authored JSON and disabled config.
    /// Installation reports unverified runtime activation, never token coverage.
    #[test]
    fn bootstrap_cli_codex_shared_hooks_preserve_authored_state_and_history() {
        use crate::integrations::bootstrap::installer::{Operation, plan_private};
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let home = PrivateHome::new();
        let root = home.0.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let input = br#"{ "description":"authored", "hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"user","timeout":7}],"matcher":"authored"}]}, "enabled":false }"#;
        std::fs::write(root.join("hooks.json"), input).unwrap();
        std::fs::write(root.join("config.toml"), b"[features]\nhooks = false\n").unwrap();
        let invoke = |flags: &[&str]| {
            let mut arguments = vec!["fixture", "codex", "--root", root.to_str().unwrap()];
            arguments.extend_from_slice(flags);
            let args = Fixture::try_parse_from(arguments).unwrap().args;
            let mut output = Vec::new();
            run_at_home(args, CliOutputFormat::Json, &mut output, &home.0).unwrap();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };
        let before = tree_snapshot(&home.0);
        assert_eq!(invoke(&["--dry-run"])["planned_outcome"], "installed");
        assert_eq!(tree_snapshot(&home.0), before);
        assert_eq!(invoke(&[])["result"], "installed");
        assert_eq!(invoke(&[])["result"], "unchanged");
        assert_eq!(
            invoke(&["--check"])["runtime_verification"],
            "not-performed"
        );
        assert_eq!(invoke(&["--uninstall"])["result"], "uninstalled");
        let after: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("hooks.json")).unwrap()).unwrap();
        assert_eq!(
            after["hooks"]["SessionStart"],
            serde_json::from_slice::<serde_json::Value>(input).unwrap()["hooks"]["SessionStart"]
        );
        assert_eq!(after["enabled"], false);
        assert_eq!(
            std::fs::read(root.join("config.toml")).unwrap(),
            b"[features]\nhooks = false\n"
        );

        std::fs::remove_file(root.join("hooks.json")).unwrap();
        let current = crate::integrations::bootstrap::compiled_manifest("codex", None).unwrap();
        let old = crate::integrations::bootstrap::compiled_history(&current)
            .pop()
            .unwrap();
        plan_private(&root, &home.0, &old, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let bytes = std::fs::read(root.join("hooks.json")).unwrap();
        let output = invoke(&[]);
        assert_eq!(output["result"], "upgraded");
        assert_eq!(
            output["changed_paths"],
            serde_json::json!(["@mez-bootstrap-receipt/codex"])
        );
        assert_eq!(std::fs::read(root.join("hooks.json")).unwrap(), bytes);
    }

    /// CLI result labels derive from accepted ownership and actual publication,
    /// not exit success or runtime activation. Read-only previews/checks preserve
    /// both trees; repairs report archived destinations without exposing bytes.
    /// Historical upgrade and pending recovery use the same production owners.
    #[test]
    fn bootstrap_cli_outcomes_distinguish_publication_preview_and_preservation() {
        use crate::integrations::bootstrap::installer::{Operation, plan_private};
        let home = PrivateHome::new();
        let root = home.0.join("vendor");
        let before = tree_snapshot(&home.0);
        for (flag, result) in [("--dry-run", "preview"), ("--check", "checked")] {
            let output = invoke_private_fixture(&home.0, &root, &[flag]).unwrap();
            assert_eq!(output["result"], result);
            assert_eq!(output["planned_outcome"], "installed");
            assert_eq!(output["runtime_verification"], "not-performed");
            assert_eq!(tree_snapshot(&home.0), before);
        }
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &[]).unwrap()["result"],
            "installed"
        );
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &[]).unwrap()["result"],
            "unchanged"
        );
        let file = root.join("extensions/mezzanine/package.json");
        std::fs::remove_file(&file).unwrap();
        let output = invoke_private_fixture(&home.0, &root, &["--check"]).unwrap();
        assert_eq!(output["result"], "checked");
        assert_eq!(output["planned_outcome"], "repaired");
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &[]).unwrap()["result"],
            "repaired"
        );
        std::fs::write(&file, b"private edit never emitted").unwrap();
        let before = tree_snapshot(&home.0);
        let output = invoke_private_fixture(&home.0, &root, &["--dry-run"]).unwrap();
        assert_eq!(
            output["planned_preserved_paths"],
            serde_json::json!(["extensions/mezzanine/package.json"])
        );
        assert_eq!(output["preserved_paths"], serde_json::json!([]));
        assert!(!output.to_string().contains("private edit never emitted"));
        assert_eq!(tree_snapshot(&home.0), before);
        let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
        let plan = plan_private(&root, &home.0, &current, Operation::Install).unwrap();
        plan.fixture_interrupt_after(1);
        assert!(plan.apply().is_err());
        for flags in [vec!["--dry-run"], vec!["--recover", "--dry-run"]] {
            let before = tree_snapshot(&home.0);
            let output = invoke_private_fixture(&home.0, &root, &flags).unwrap();
            assert_eq!(output["result"], "preview");
            assert_eq!(output["recovery_pending"], true);
            assert_eq!(
                output["planned_preserved_paths"],
                serde_json::json!(["extensions/mezzanine/package.json"])
            );
            assert_eq!(output["preserved_paths"], serde_json::json!([]));
            assert_eq!(tree_snapshot(&home.0), before);
        }
        let output = invoke_private_fixture(&home.0, &root, &["--recover"]).unwrap();
        assert_eq!(output["result"], "recovered");
        assert_eq!(
            output["preserved_paths"],
            serde_json::json!(["extensions/mezzanine/package.json"])
        );
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &["--recover"]).unwrap()["result"],
            "unchanged"
        );
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &["--uninstall"]).unwrap()["result"],
            "uninstalled"
        );
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &["--uninstall"]).unwrap()["result"],
            "unchanged"
        );

        let home = PrivateHome::new();
        let root = home.0.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let old = crate::integrations::bootstrap::compiled_history(&current).remove(0);
        plan_private(&root, &home.0, &old, Operation::Install)
            .unwrap()
            .apply()
            .unwrap();
        let before = tree_snapshot(&home.0);
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &["--dry-run"]).unwrap()["planned_outcome"],
            "upgraded"
        );
        assert_eq!(tree_snapshot(&home.0), before);
        assert_eq!(
            invoke_private_fixture(&home.0, &root, &[]).unwrap()["result"],
            "upgraded"
        );
    }

    /// Plain reports summarize the same captured operation without dumping JSON
    /// or claiming vendor activation. Rendering escapes control-bearing display
    /// paths; invalid ownership returns no success text or archived payload bytes.
    #[test]
    fn bootstrap_cli_plain_reports_are_concise_escaped_and_failure_safe() {
        let home = PrivateHome::new();
        let root = home.0.join("vendor");
        let args = BootstrapCliArgs {
            harness: "pi".into(),
            vendor_version: None,
            root: Some(root.clone()),
            dry_run: true,
            check: false,
            uninstall: false,
            recover: false,
        };
        let mut output = Vec::new();
        let before = tree_snapshot(&home.0);
        run_at_home(args.clone(), CliOutputFormat::Plain, &mut output, &home.0).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("bootstrap pi: preview\n"));
        assert!(text.contains("Planned outcome: installed (no writes)"));
        assert!(text.contains("(explicit)"));
        assert!(text.contains("not verified"));
        assert!(text.lines().count() <= 5);
        assert_eq!(tree_snapshot(&home.0), before);
        let mut actual = args.clone();
        actual.dry_run = false;
        let mut output = Vec::new();
        run_at_home(actual.clone(), CliOutputFormat::Plain, &mut output, &home.0).unwrap();
        assert!(
            String::from_utf8(output)
                .unwrap()
                .starts_with("bootstrap pi: installed\n")
        );

        let mut report = invoke_private_fixture(&home.0, &root, &["--dry-run"]).unwrap();
        report["scope_root"] = "\u{1b}[2J\ncontrol-bearing path".into();
        let mut output = Vec::new();
        write_report(&mut output, CliOutputFormat::Plain, &report).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(!text.contains('\u{1b}'));
        assert!(text.contains("\\ncontrol-bearing path"));
        assert_eq!(text.lines().count(), 5);

        actual.harness = "codex".into();
        std::fs::write(
            root.join("hooks.json"),
            b"authored callback must never leak",
        )
        .unwrap();
        for format in [CliOutputFormat::Plain, CliOutputFormat::Json] {
            let mut output = Vec::new();
            let before = tree_snapshot(&home.0);
            assert!(run_at_home(actual.clone(), format, &mut output, &home.0).is_err());
            assert!(output.is_empty());
            assert_eq!(tree_snapshot(&home.0), before);
        }
        let mut absent = args;
        absent.harness = "claude".into();
        let mut output = Vec::new();
        run_at_home(absent, CliOutputFormat::Plain, &mut output, &home.0).unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "bootstrap claude: unavailable\nNo compiled adapter; no files changed.\n"
        );
    }

    /// Observes the sole test-owned namespace after real engine publication.
    /// The pathname is fixture evidence only; production never discovers journal
    /// authority by enumerating HOME or choosing an arbitrary namespace.
    fn private_fixture_journal(home: &std::path::Path) -> PathBuf {
        let entries = std::fs::read_dir(home.join(".config/mezzanine/bootstrap"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        assert_eq!(entries.len(), 1);
        entries[0].join("journal.json")
    }

    /// The former legacy route rejects a real private-only interrupted install;
    /// all normal activated intents must instead preview and settle that exact
    /// accepted intent. Read-only CLI snapshots preserve both trees, uninstall
    /// follows original completion, and repeats neither recover nor publish.
    #[test]
    fn bootstrap_cli_private_pending_intents_share_the_public_owner() {
        use crate::integrations::bootstrap::installer::{Operation, plan_private};
        let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
        for intent in ["install", "uninstall", "recover"] {
            let home = PrivateHome::new();
            let root = home.0.join(".pi/agent");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("authored"), b"preserved").unwrap();
            let accepted = plan_private(&root, &home.0, &current, Operation::Install).unwrap();
            accepted.fixture_interrupt_after(1);
            assert!(accepted.apply().is_err());
            assert!(!root.join(".mez-bootstrap-journal").exists());
            assert!(
                crate::integrations::bootstrap::installer::plan(
                    &root,
                    &current,
                    Operation::Install
                )
                .is_err(),
                "former legacy-only route cannot admit private pending intent"
            );
            let before = tree_snapshot(&home.0);
            for flags in [
                vec!["--check"],
                vec!["--dry-run"],
                vec!["--uninstall", "--dry-run"],
                vec!["--recover", "--dry-run"],
            ] {
                let output = invoke_private_fixture(&home.0, &root, &flags).unwrap();
                assert_eq!(output["recovery_pending"], true);
                assert_eq!(output["recovered"], false);
                assert!(!output["changed_paths"].as_array().unwrap().is_empty());
                assert_eq!(tree_snapshot(&home.0), before);
            }
            let flags: &[&str] = match intent {
                "install" => &[],
                "uninstall" => &["--uninstall"],
                _ => &["--recover"],
            };
            let output = invoke_private_fixture(&home.0, &root, flags).unwrap();
            assert_eq!(output["recovered"], true);
            assert_eq!(output["recovery_pending"], false);
            assert_eq!(
                output["result"],
                match intent {
                    "install" => "unchanged",
                    "uninstall" => "uninstalled",
                    _ => "recovered",
                }
            );
            assert!(!private_fixture_journal(&home.0).exists());
            assert_eq!(
                crate::integrations::bootstrap::installer::fixture_private_receipt(
                    &root, &home.0, "pi"
                )
                .unwrap()
                .is_some(),
                intent != "uninstall"
            );
            assert!(!root.join("mez-bootstrap-ownership-pi.json").exists());
            assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
            let repeated = invoke_private_fixture(&home.0, &root, flags).unwrap();
            assert_eq!(repeated["recovered"], false);
            assert_eq!(repeated["recovery_pending"], false);
            assert!(repeated["changed_paths"].as_array().unwrap().is_empty());
        }
    }

    /// Divergent private and legacy copies cannot be ignored by any public
    /// maintenance/install/check path. Rejection occurs without journal changes,
    /// artifact progress or silent winner selection, even for actual operations.
    #[test]
    fn bootstrap_cli_private_copy_disagreement_rejects_every_intent_unchanged() {
        use crate::integrations::bootstrap::installer::{Operation, plan_private};
        let home = PrivateHome::new();
        let root = home.0.join(".pi/agent");
        std::fs::create_dir_all(&root).unwrap();
        let current = crate::integrations::bootstrap::compiled_manifest("pi", None).unwrap();
        let accepted = plan_private(&root, &home.0, &current, Operation::Install).unwrap();
        accepted.fixture_interrupt_after(1);
        assert!(accepted.apply().is_err());
        let bytes = std::fs::read(private_fixture_journal(&home.0)).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        std::fs::write(
            root.join(".mez-bootstrap-journal"),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        let before = tree_snapshot(&home.0);
        for flags in [
            vec!["--check"],
            vec!["--dry-run"],
            vec!["--recover", "--dry-run"],
            vec![],
            vec!["--uninstall"],
            vec!["--recover"],
        ] {
            let error = invoke_private_fixture(&home.0, &root, &flags).unwrap_err();
            assert!(error.to_string().contains("copies disagree"));
            assert_eq!(tree_snapshot(&home.0), before);
        }
    }

    /// Recovery/check/noop uninstall against an absent selected vendor root
    /// must not materialize either vendor or private state. Actual recovery is
    /// a truthful no-op with the same bounded admission as recovery preview.
    #[test]
    fn bootstrap_cli_private_absent_maintenance_creates_neither_tree() {
        let home = PrivateHome::new();
        let root = home.0.join("absent/vendor");
        let before = tree_snapshot(&home.0);
        for flags in [
            vec!["--recover", "--dry-run"],
            vec!["--recover"],
            vec!["--uninstall", "--dry-run"],
            vec!["--uninstall"],
        ] {
            let output = invoke_private_fixture(&home.0, &root, &flags).unwrap();
            assert_eq!(output["recovered"], false);
            assert_eq!(output["recovery_pending"], false);
            assert!(output["changed_paths"].as_array().unwrap().is_empty());
            assert_eq!(tree_snapshot(&home.0), before);
        }
        for flags in [vec!["--check"], vec!["--dry-run"]] {
            assert!(
                !invoke_private_fixture(&home.0, &root, &flags).unwrap()["changed_paths"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(tree_snapshot(&home.0), before);
        }
    }

    /// Bare bootstrap installs at its captured automatic root, while explicit
    /// dry-run keeps even absent roots untouched. Repetition is unchanged;
    /// uninstall preview preserves exact installed artifacts before removal.
    #[test]
    fn bootstrap_default_install_and_explicit_dry_run_are_distinct() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let home = std::env::temp_dir().join(format!(
            "mez-bootstrap-intents-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&home).unwrap();
        let root = home.join(".pi/agent");
        let invoke = |flags: &[&str]| {
            let mut arguments = vec!["fixture", "pi"];
            arguments.extend_from_slice(flags);
            let parsed = Fixture::try_parse_from(arguments).unwrap();
            let mut output = Vec::new();
            run_with_root_selector(
                parsed.args,
                CliOutputFormat::Json,
                &mut output,
                crate::integrations::bootstrap::compiled_manifest("pi", None),
                |name, explicit| {
                    crate::integrations::bootstrap::roots::resolve_with_environment(
                        name,
                        explicit,
                        |key| (key == "HOME").then(|| home.clone().into_os_string()),
                        false,
                    )
                },
                || Ok(home.clone()),
            )
            .unwrap();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };
        assert_eq!(invoke(&["--dry-run"])["dry_run"], true);
        assert!(!home.join(".pi").exists());
        assert_eq!(invoke(&[])["operation"], "install");
        assert!(root.join("extensions/mezzanine/index.mjs").is_file());
        assert!(invoke(&[])["changed_paths"].as_array().unwrap().is_empty());
        let owned = root.join("extensions/mezzanine/index.mjs");
        let bytes = std::fs::read(&owned).unwrap();
        std::fs::remove_file(&owned).unwrap();
        for preview in ["--check", "--dry-run"] {
            let output = invoke(&[preview]);
            assert_eq!(
                output["changed_paths"],
                serde_json::json!(["extensions/mezzanine/index.mjs"])
            );
            assert!(!owned.exists());
        }
        invoke(&[]);
        assert_eq!(std::fs::read(&owned).unwrap(), bytes);
        assert!(invoke(&[])["changed_paths"].as_array().unwrap().is_empty());
        assert_eq!(
            invoke(&["--uninstall", "--dry-run"])["operation"],
            "uninstall"
        );
        assert!(root.join("extensions/mezzanine/index.mjs").is_file());
        invoke(&["--uninstall"]);
        assert!(!root.join("extensions/mezzanine/index.mjs").exists());
        assert!(Fixture::try_parse_from(["fixture", "pi", "--plan"]).is_err());
        assert!(Fixture::try_parse_from(["fixture", "pi", "--apply"]).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Intent grammar removes obsolete flags rather than aliases them, rejects
    /// ambiguous maintenance/check combinations, and admits explicit previews
    /// for every mutating intent without a root or version prerequisite.
    #[test]
    fn bootstrap_intent_grammar_has_no_legacy_apply_or_plan_alias() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        for flags in [
            vec!["--dry-run"],
            vec!["--recover", "--dry-run"],
            vec!["--uninstall", "--dry-run"],
            vec!["--check"],
        ] {
            assert!(Fixture::try_parse_from(["fixture", "pi"].into_iter().chain(flags)).is_ok());
        }
        for flags in [
            vec!["--plan"],
            vec!["--apply"],
            vec!["--recover", "--uninstall"],
            vec!["--check", "--dry-run"],
            vec!["--check", "--uninstall"],
            vec!["--check", "--recover"],
        ] {
            assert!(Fixture::try_parse_from(["fixture", "pi"].into_iter().chain(flags)).is_err());
        }
        let help = <Fixture as clap::CommandFactory>::command()
            .render_long_help()
            .to_string();
        assert!(help.contains("--dry-run"));
        assert!(!help.contains("--apply") && !help.contains("--plan"));
    }

    /// A real rooted journal with compiled test intent previews through the
    /// production CLI without locking or publication. Typed output distinguishes
    /// pending work from no journal, and actual recovery subsequently settles
    /// the same bounded changes. Raw JSON is test evidence, never public input.
    #[test]
    fn bootstrap_recovery_dry_run_preserves_tree_and_reports_pending() {
        use crate::integrations::bootstrap::{
            installer::Manifest,
            reconciliation::{Artifact, Entry},
        };
        use std::os::unix::fs::MetadataExt;
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        #[derive(serde::Serialize)]
        struct TestReceipt<'a> {
            schema: u32,
            manifest: &'a Manifest,
        }
        let root = std::env::temp_dir().join(format!(
            "mez-cli-recovery-preview-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let home = PrivateHome::new();
        let manifest = Manifest {
            harness: "codex".into(),
            revision: 1,
            vendor_version: "fixture-only".into(),
            entries: vec![Entry {
                path: "owned".into(),
                artifact: Artifact::File {
                    bytes: b"owned".to_vec(),
                },
            }],
        };
        let metadata = std::fs::metadata(&root).unwrap();
        let receipt = serde_json::to_vec(&TestReceipt {
            schema: 1,
            manifest: &manifest,
        })
        .unwrap();
        let journal = serde_json::to_vec(&serde_json::json!({"version":2,"root_device":metadata.dev(),"root_inode":metadata.ino(),"intent":{"manifest":manifest,"previous":null,"operation":"Install"},"changes":[{"path":"owned","before":null,"after":b"owned".to_vec()},{"path":"mez-bootstrap-ownership-codex.json","before":null,"after":receipt}]})).unwrap();
        std::fs::write(root.join(".mez-bootstrap-journal"), &journal).unwrap();
        let invoke = |flags: &[&str]| {
            let mut arguments = vec!["fixture", "codex", "--root", root.to_str().unwrap()];
            arguments.extend_from_slice(flags);
            let parsed = Fixture::try_parse_from(arguments).unwrap();
            let mut output = Vec::new();
            run_manifest_at_home(
                parsed.args,
                CliOutputFormat::Json,
                &mut output,
                Some(manifest.clone()),
                &home.0,
            )
            .unwrap();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };
        let preview = invoke(&["--recover", "--dry-run"]);
        assert_eq!(preview["operation"], "recover");
        assert_eq!(preview["dry_run"], true);
        assert_eq!(preview["recovery_pending"], true);
        assert_eq!(preview["recovered"], false);
        assert_eq!(preview["changed_paths"].as_array().unwrap().len(), 2);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(root.join(".mez-bootstrap-journal")).unwrap(),
            journal
        );
        for intent in ["--dry-run", "--check"] {
            let preview = invoke(&[intent]);
            assert_eq!(preview["recovery_pending"], true);
            assert_eq!(preview["recovered"], false);
            assert_eq!(preview["changed_paths"].as_array().unwrap().len(), 3);
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            assert_eq!(
                std::fs::read(root.join(".mez-bootstrap-journal")).unwrap(),
                journal
            );
        }
        assert_eq!(invoke(&["--recover"])["recovered"], true);
        assert_eq!(std::fs::read(root.join("owned")).unwrap(), b"owned");
        assert_eq!(
            invoke(&["--recover", "--dry-run"])["recovery_pending"],
            false
        );
        // All effects already equal their after state, but the original journal
        // still requires settlement even when requested reconciliation is noop.
        std::fs::write(root.join(".mez-bootstrap-journal"), &journal).unwrap();
        let installed = invoke(&[]);
        assert_eq!(installed["recovered"], true);
        assert_eq!(installed["recovery_pending"], false);
        assert_eq!(installed["operation"], "install");
        let repeated = invoke(&[]);
        assert_eq!(repeated["recovered"], false);
        assert_eq!(repeated["recovery_pending"], false);
        assert!(repeated["changed_paths"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Normal compiled harness commands can select their user roots without
    /// --root, vendor execution or daemon discovery. Injected HOME/directory
    /// overrides keep fixture writes isolated; check observes and apply uses
    /// the exact selected tree, while authored siblings and repeat remain safe.
    #[test]
    fn bootstrap_automatic_root_selection_drives_compiled_cli() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let home = std::env::temp_dir().join(format!(
            "mez-auto-root-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&home).unwrap();
        for (harness, suffix, variable) in [
            ("pi", ".pi/agent", "PI_CODING_AGENT_DIR"),
            ("opencode", ".config/opencode", "OPENCODE_CONFIG_DIR"),
            ("codex", ".codex", "CODEX_HOME"),
        ] {
            let root = home.join(suffix);
            assert!(!root.exists());
            for intent in ["--check", "", "--check"] {
                let mut arguments = vec!["fixture", harness];
                if !intent.is_empty() {
                    arguments.push(intent);
                }
                let parsed = Fixture::try_parse_from(arguments).unwrap();
                let manifest = crate::integrations::bootstrap::compiled_manifest(harness, None);
                let mut output = Vec::new();
                run_with_root_selector(
                    parsed.args,
                    CliOutputFormat::Json,
                    &mut output,
                    manifest,
                    |name, explicit| {
                        crate::integrations::bootstrap::roots::resolve_with_environment(
                            name,
                            explicit,
                            |key| (key == "HOME").then(|| home.clone().into_os_string()),
                            false,
                        )
                    },
                    || Ok(home.clone()),
                )
                .unwrap();
                let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(output["scope_root"], root.to_str().unwrap());
                assert_eq!(output["root_source"], "vendor-default");
                if intent.is_empty() {
                    assert!(
                        crate::integrations::bootstrap::installer::fixture_private_receipt(
                            &root, &home, harness
                        )
                        .unwrap()
                        .is_some()
                    );
                    assert!(
                        !root
                            .join(format!("mez-bootstrap-ownership-{harness}.json"))
                            .exists()
                    );
                    std::fs::write(root.join("authored"), b"preserved").unwrap();
                } else if !root.exists() {
                    assert!(!output["changed_paths"].as_array().unwrap().is_empty());
                }
            }
            let parsed = Fixture::try_parse_from(["fixture", harness, "--check"]).unwrap();
            let mut output = Vec::new();
            run_with_root_selector(
                parsed.args,
                CliOutputFormat::Json,
                &mut output,
                crate::integrations::bootstrap::compiled_manifest(harness, None),
                |name, explicit| {
                    crate::integrations::bootstrap::roots::resolve_with_environment(
                        name,
                        explicit,
                        |key| {
                            assert_eq!(key, variable);
                            Some(root.clone().into_os_string())
                        },
                        false,
                    )
                },
                || Ok(home.clone()),
            )
            .unwrap();
            let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(output["root_source"], variable);
            assert!(output["changed_paths"].as_array().unwrap().is_empty());
            assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Invalid captured private HOME must reject without creating either tree.
    /// Root errors and absent adapters must not invoke private HOME selection;
    /// injected failures stand in for missing HOME without process-env mutation.
    #[test]
    fn bootstrap_private_home_invalid_or_missing_is_non_mutating() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let home = PrivateHome::new();
        let root = home.0.join("vendor");
        std::fs::create_dir(&root).unwrap();
        let before = tree_snapshot(&home.0);
        for invalid in [
            PathBuf::from("relative"),
            home.0.join("../home"),
            home.0.join("absent"),
        ] {
            let parsed = Fixture::try_parse_from([
                "fixture",
                "pi",
                "--root",
                root.to_str().unwrap(),
                "--check",
            ])
            .unwrap();
            assert!(
                run_with_root_selector(
                    parsed.args,
                    CliOutputFormat::Json,
                    &mut Vec::new(),
                    crate::integrations::bootstrap::compiled_manifest("pi", None),
                    crate::integrations::bootstrap::roots::resolve,
                    || Ok(invalid)
                )
                .is_err()
            );
            assert_eq!(tree_snapshot(&home.0), before);
        }
        let parsed =
            Fixture::try_parse_from(["fixture", "pi", "--root", root.to_str().unwrap(), "--check"])
                .unwrap();
        assert!(
            run_with_root_selector(
                parsed.args,
                CliOutputFormat::Json,
                &mut Vec::new(),
                crate::integrations::bootstrap::compiled_manifest("pi", None),
                crate::integrations::bootstrap::roots::resolve,
                || Err(MezError::invalid_args("fixture HOME unavailable"))
            )
            .is_err()
        );
        assert_eq!(tree_snapshot(&home.0), before);
    }

    /// Guarded child owns the real production environment path for all public
    /// bootstrap intents. It never executes a vendor/provider, and missing HOME
    /// remains an admission error even with an explicit valid vendor-root override.
    #[test]
    fn bootstrap_process_environment_private_intent_fixture() {
        let Some(root) = std::env::var_os("MEZ_TEST_PRIVATE_ROOT").map(PathBuf::from) else {
            return;
        };
        let intent = std::env::var("MEZ_TEST_PRIVATE_INTENT").unwrap();
        let harness = std::env::var("MEZ_TEST_PRIVATE_HARNESS").unwrap();
        let args = BootstrapCliArgs {
            harness: harness.clone(),
            vendor_version: None,
            root: (intent == "missing-home").then(|| root.clone()),
            dry_run: intent == "dry-run",
            check: matches!(intent.as_str(), "check" | "missing-home"),
            uninstall: intent == "uninstall",
            recover: intent == "recover",
        };
        let mut output = Vec::new();
        let result = run(args, CliOutputFormat::Json, &mut output);
        if intent == "missing-home" {
            assert!(result.unwrap_err().to_string().contains("HOME is not set"));
            assert!(output.is_empty());
            return;
        }
        result.unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["scope_root"], root.to_str().unwrap());
        assert_eq!(value["root_source"], "vendor-default");
        assert_eq!(value["supported"], true);
    }

    /// Real child process HOME lookup drives bare first install, repeat/check,
    /// uninstall and absent recovery for every currently compiled adapter. Fresh
    /// OpenCode starts with no shared .config ancestor; read-only snapshots and
    /// actual journal placement qualify the public routing without fixture tokens.
    #[test]
    fn bootstrap_process_environment_private_intents_are_isolated() {
        for (harness, suffix) in [
            ("pi", ".pi/agent"),
            ("opencode", ".config/opencode"),
            ("codex", ".codex"),
        ] {
            let home = PrivateHome::new();
            let root = home.0.join(suffix);
            let child = |intent: &str| {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", "cli::bootstrap::tests::bootstrap_process_environment_private_intent_fixture", "--quiet"])
                    .env_clear().env("MEZ_TEST_PRIVATE_ROOT", &root).env("MEZ_TEST_PRIVATE_INTENT", intent)
                    .env("MEZ_TEST_PRIVATE_HARNESS", harness).stdin(std::process::Stdio::null());
                if intent != "missing-home" {
                    command.env("HOME", &home.0);
                }
                let output = command.output().unwrap();
                assert!(
                    output.status.success(),
                    "{harness}/{intent}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            let before = tree_snapshot(&home.0);
            for intent in ["dry-run", "check", "recover", "uninstall", "missing-home"] {
                child(intent);
                assert_eq!(tree_snapshot(&home.0), before);
            }
            child("install");
            assert!(
                crate::integrations::bootstrap::installer::fixture_private_receipt(
                    &root, &home.0, harness
                )
                .unwrap()
                .is_some()
            );
            assert!(
                !root
                    .join(format!("mez-bootstrap-ownership-{harness}.json"))
                    .exists()
            );
            assert!(!root.join(".mez-bootstrap-journal").exists());
            assert!(!private_fixture_journal(&home.0).exists());
            let before = tree_snapshot(&home.0);
            for intent in ["install", "check", "dry-run", "recover"] {
                child(intent);
                assert_eq!(tree_snapshot(&home.0), before);
            }
            child("uninstall");
            assert!(
                crate::integrations::bootstrap::installer::fixture_private_receipt(
                    &root, &home.0, harness
                )
                .unwrap()
                .is_none()
            );
            assert!(
                !root
                    .join(format!("mez-bootstrap-ownership-{harness}.json"))
                    .exists()
            );
            assert!(!private_fixture_journal(&home.0).exists());
        }
    }

    /// Guarded self-process fixture exercises the production HOME/vendor-env
    /// lookup without mutating another test's environment or launching a vendor.
    /// Outside its explicit private fixture environment it remains a no-op.
    #[test]
    fn bootstrap_process_environment_root_fixture() {
        if std::env::var_os("MEZ_TEST_BOOT_ROOT").is_none() {
            return;
        }
        let args = BootstrapCliArgs {
            harness: "pi".into(),
            vendor_version: None,
            root: None,
            dry_run: false,
            check: true,
            uninstall: false,
            recover: false,
        };
        let mut output = Vec::new();
        run(args, CliOutputFormat::Json, &mut output).unwrap();
        let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            output["scope_root"],
            std::env::var("MEZ_TEST_BOOT_ROOT").unwrap()
        );
        assert_eq!(
            output["root_source"],
            std::env::var("MEZ_TEST_BOOT_SOURCE").unwrap()
        );
    }

    /// Ordinary production selection uses only the child process's isolated
    /// routing environment. Check creates no installer state; HOME default and
    /// vendor override work with no --root, daemon, vendor or current-dir hint.
    #[test]
    fn bootstrap_process_environment_selects_default_and_override_read_only() {
        let home = std::env::temp_dir().join(format!(
            "mez-process-root-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let root = home.join(".pi/agent");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("authored"), b"unchanged").unwrap();
        for source in ["vendor-default", "PI_CODING_AGENT_DIR"] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "cli::bootstrap::tests::bootstrap_process_environment_root_fixture",
                    "--quiet",
                ])
                .env_clear()
                .env("HOME", &home)
                .env("MEZ_TEST_BOOT_ROOT", &root)
                .env("MEZ_TEST_BOOT_SOURCE", source)
                .stdin(std::process::Stdio::null());
            if source == "PI_CODING_AGENT_DIR" {
                child.env(source, &root);
            }
            assert!(child.status().unwrap().success());
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
            assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"unchanged");
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    /// Invalid selected values do not fallback to a user tree; absent adapters
    /// do not even invoke the selector and remain honest no-mutation diagnostics.
    #[test]
    fn bootstrap_root_selection_invalid_override_and_absent_adapter_are_inert() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let parsed = Fixture::try_parse_from(["fixture", "pi", "--check"]).unwrap();
        assert!(
            run_with_root_selector(
                parsed.args,
                CliOutputFormat::Json,
                &mut Vec::new(),
                crate::integrations::bootstrap::compiled_manifest("pi", None),
                |name, explicit| {
                    crate::integrations::bootstrap::roots::resolve_with_environment(
                        name,
                        explicit,
                        |key| {
                            assert_eq!(key, "PI_CODING_AGENT_DIR");
                            Some("relative".into())
                        },
                        false,
                    )
                },
                || panic!("invalid root selected private HOME"),
            )
            .is_err()
        );
        let parsed = Fixture::try_parse_from(["fixture", "claude", "--check"]).unwrap();
        let mut output = Vec::new();
        run_with_root_selector(
            parsed.args,
            CliOutputFormat::Json,
            &mut output,
            None,
            |_, _| panic!("absent adapter selected root"),
            || panic!("absent adapter selected private HOME"),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()["supported"],
            false
        );
    }

    /// Non-UTF8 Unix root bytes remain exact during selection/publication, while
    /// diagnostic JSON may use replacement characters. Check/apply/repeat must
    /// not panic, retarget a display spelling, or change authored sibling bytes.
    #[test]
    fn bootstrap_non_utf8_root_publication_keeps_exact_os_path() {
        use std::os::unix::ffi::OsStringExt;
        let home = PrivateHome::new();
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let parent = std::env::temp_dir().join(format!(
            "mez-nonutf8-root-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let root = parent.join(std::ffi::OsString::from_vec(b"vendor-\xff".to_vec()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("authored"), b"preserved").unwrap();
        for intent in ["--check", "", "--check"] {
            let mut arguments = vec![
                std::ffi::OsString::from("fixture"),
                "pi".into(),
                "--root".into(),
                root.clone().into_os_string(),
            ];
            if !intent.is_empty() {
                arguments.push(intent.into());
            }
            let parsed = Fixture::try_parse_from(arguments).unwrap();
            let mut output = Vec::new();
            run_at_home(parsed.args, CliOutputFormat::Json, &mut output, &home.0).unwrap();
            let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(output["scope_root"], root.to_string_lossy().as_ref());
            assert_eq!(output["root_source"], "explicit");
            if intent.is_empty() {
                assert!(root.join("extensions/mezzanine/index.mjs").is_file());
            }
            assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        }
        assert!(!std::path::Path::new(root.to_string_lossy().as_ref()).exists());
        std::fs::remove_dir_all(parent).unwrap();
    }

    /// Public check/plan uses the compiled registry but needs no writable root,
    /// lock, artifact parent or daemon. Missing explicit roots fail without
    /// creating directories; root materialization remains a separate next phase.
    #[test]
    fn bootstrap_public_check_and_plan_leave_root_tree_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let home = PrivateHome::new();
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let root = std::env::temp_dir().join(format!(
            "mez-cli-inspect-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("authored.json"), b"{\"user\":true}\n").unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
        for harness in ["pi", "opencode", "codex"] {
            for intent in ["--check", "--dry-run"] {
                let parsed = Fixture::try_parse_from([
                    "fixture",
                    harness,
                    "--root",
                    root.to_str().unwrap(),
                    intent,
                ])
                .unwrap();
                let mut output = Vec::new();
                run_at_home(parsed.args, CliOutputFormat::Json, &mut output, &home.0).unwrap();
                let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(value["supported"], true);
                assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
                assert_eq!(
                    std::fs::read(root.join("authored.json")).unwrap(),
                    b"{\"user\":true}\n"
                );
                assert_eq!(
                    std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                    0o500
                );
            }
        }
        let missing = root.join("missing/vendor");
        let parsed = Fixture::try_parse_from([
            "fixture",
            "pi",
            "--root",
            missing.to_str().unwrap(),
            "--check",
        ])
        .unwrap();
        run_at_home(parsed.args, CliOutputFormat::Json, &mut Vec::new(), &home.0).unwrap();
        assert!(!root.join("missing").exists());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Public Pi installation accepts docs-based best-effort support without
    /// a release pin. Explicit roots still gate filesystem access, while the
    /// manifest owns only extension artifacts and not vendor settings.
    #[test]
    fn bootstrap_pi_candidate_is_not_installation_certification() {
        let home = PrivateHome::new();
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let root = std::env::temp_dir().join(format!(
            "mez-pi-public-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let parsed =
            Fixture::try_parse_from(["fixture", "pi", "--root", root.to_str().unwrap()]).unwrap();
        let mut output = Vec::new();
        run_at_home(parsed.args, CliOutputFormat::Json, &mut output, &home.0).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["supported"], true);
        assert!(root.join("extensions/mezzanine/index.mjs").is_file());
        let parsed = Fixture::try_parse_from([
            "fixture",
            "pi",
            "--vendor-version",
            "future-local",
            "--root",
            root.to_str().unwrap(),
            "--check",
        ])
        .unwrap();
        run_at_home(parsed.args, CliOutputFormat::Json, &mut Vec::new(), &home.0).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let parsed =
            Fixture::try_parse_from(["fixture", "pi", "--root", root.to_str().unwrap()]).unwrap();
        run_at_home(parsed.args, CliOutputFormat::Json, &mut Vec::new(), &home.0).unwrap();
        assert!(root.join("extensions/mezzanine/index.mjs").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Every retired-harness intent fails during argument admission, before
    /// root access or daemon work. Other candidates remain inspectable.
    #[test]
    fn bootstrap_retired_gemini_rejects_every_intent() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        for flag in ["--dry-run", "--check", "--uninstall", "--recover"] {
            assert!(
                Fixture::try_parse_from([
                    "fixture",
                    "gemini",
                    "--root",
                    "/missing/retired/root",
                    flag,
                ])
                .is_err()
            );
        }
        assert!(Fixture::try_parse_from(["fixture", "gemini"]).is_err());
        for harness in ["claude", "codex", "copilot", "opencode", "cursor"] {
            assert!(Fixture::try_parse_from(["fixture", harness, "--dry-run"]).is_ok());
        }
    }

    /// Compiled fixture admission drives plan, apply, repeat and uninstall without
    /// daemon access. The fixture is not advertised as a certified vendor release.
    #[test]
    fn bootstrap_cli_compiled_fixture_drives_owned_installation() {
        use crate::integrations::bootstrap::{
            installer::Manifest,
            reconciliation::{Artifact, Entry},
        };
        use std::os::unix::fs::PermissionsExt;
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let root = std::env::temp_dir().join(format!(
            "mez-bootstrap-cli-{}",
            crate::storage::token_usage::new_token_usage_event_id()
        ));
        let home = PrivateHome::new();
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manifest = Manifest {
            harness: "codex".into(),
            revision: 1,
            vendor_version: "fixture-only".into(),
            entries: vec![Entry {
                path: "owned.json".into(),
                artifact: Artifact::File {
                    bytes: b"{}\n".to_vec(),
                },
            }],
        };
        let invoke = |flag: &str| {
            let mut arguments = vec![
                "fixture",
                "codex",
                "--vendor-version",
                "fixture-only",
                "--root",
                root.to_str().unwrap(),
            ];
            if !flag.is_empty() {
                arguments.push(flag);
            }
            let parsed = Fixture::try_parse_from(arguments).unwrap();
            let mut output = Vec::new();
            run_manifest_at_home(
                parsed.args,
                CliOutputFormat::Json,
                &mut output,
                Some(manifest.clone()),
                &home.0,
            )
            .unwrap();
            serde_json::from_slice::<serde_json::Value>(&output).unwrap()
        };
        assert_eq!(
            invoke("--dry-run")["changed_paths"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(!root.join("owned.json").exists());
        invoke("");
        assert_eq!(std::fs::read(root.join("owned.json")).unwrap(), b"{}\n");
        assert!(
            invoke("--check")["changed_paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(invoke("")["changed_paths"].as_array().unwrap().is_empty());
        invoke("--uninstall");
        assert!(!root.join("owned.json").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Candidates can be inspected without a daemon or filesystem mutation, but
    /// must not be installed by guessing released schemas or event field names.
    #[test]
    fn bootstrap_cli_refuses_uncertified_mutation_before_root_access() {
        #[derive(Parser)]
        struct Fixture {
            #[command(flatten)]
            args: BootstrapCliArgs,
        }
        let parsed = Fixture::try_parse_from([
            "fixture",
            "claude",
            "--root",
            "/missing/bootstrap/root",
            "--dry-run",
        ])
        .unwrap();
        let mut output = Vec::new();
        run(parsed.args, CliOutputFormat::Json, &mut output).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["supported"], false);
        let parsed =
            Fixture::try_parse_from(["fixture", "claude", "--root", "/missing/bootstrap/root"])
                .unwrap();
        assert!(run(parsed.args, CliOutputFormat::Json, &mut Vec::new()).is_err());
        assert!(Fixture::try_parse_from(["fixture", "codex", "--check", "--dry-run"]).is_err());
    }
}
