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
    /// Remove receipted artifacts only when present owned content is unchanged.
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
    )
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
        let root = &selected.path;
        let mut recovered = false;
        let mut recovery_pending = false;
        let mut changed_paths = Vec::new();
        let operation = if args.recover {
            if args.dry_run {
                if let Some(paths) =
                    crate::integrations::bootstrap::installer::preview_recovery(root, &manifest)?
                {
                    recovery_pending = true;
                    changed_paths = paths;
                }
            } else {
                recovered = crate::integrations::bootstrap::installer::recover(root, &manifest)?;
            }
            "recover"
        } else {
            use crate::integrations::bootstrap::installer::{Operation, plan};
            let plan = plan(
                root,
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
        return super::write_json_or_plain(stdout, format, &serde_json::json!({
            "harness":args.harness,"vendor_version":args.vendor_version,"operation":operation,
            "scope_root":root.to_string_lossy(),"root_source":selected.source,
            "supported":true,"manifest_revision":manifest.revision,"changed_paths":changed_paths,"recovered":recovered,
            "dry_run":args.dry_run,"recovery_pending":recovery_pending,
            "support":"best-effort",
            "guidance":"Installation is observational only. Use ordinary vendor commands and preserve vendor review/disabled policy. Automatic enrollment and accounting capabilities may still be unavailable; no Mez vendor-launch wrappers exist",
        }).to_string());
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
        "lifecycle":"unavailable", "usage":"unavailable",
        "reason":"No compiled adapter manifest; no vendor configuration, credentials or hook trust changed",
    }).to_string();
    super::write_json_or_plain(stdout, format, &output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
            run_with_manifest(
                parsed.args,
                CliOutputFormat::Json,
                &mut output,
                Some(manifest.clone()),
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
            assert_eq!(preview["changed_paths"].as_array().unwrap().len(), 2);
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
                )
                .unwrap();
                let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(output["scope_root"], root.to_str().unwrap());
                assert_eq!(output["root_source"], "vendor-default");
                if intent.is_empty() {
                    assert!(
                        root.join(format!("mez-bootstrap-ownership-{harness}.json"))
                            .is_file()
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
            )
            .unwrap();
            let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(output["root_source"], variable);
            assert!(output["changed_paths"].as_array().unwrap().is_empty());
            assert_eq!(std::fs::read(root.join("authored")).unwrap(), b"preserved");
        }
        std::fs::remove_dir_all(home).unwrap();
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
                }
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
            run(parsed.args, CliOutputFormat::Json, &mut output).unwrap();
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
                run(parsed.args, CliOutputFormat::Json, &mut output).unwrap();
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
        run(parsed.args, CliOutputFormat::Json, &mut Vec::new()).unwrap();
        assert!(!root.join("missing").exists());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Public Pi installation accepts docs-based best-effort support without
    /// a release pin. Explicit roots still gate filesystem access, while the
    /// manifest owns only extension artifacts and not vendor settings.
    #[test]
    fn bootstrap_pi_candidate_is_not_installation_certification() {
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
        run(parsed.args, CliOutputFormat::Json, &mut output).unwrap();
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
        run(parsed.args, CliOutputFormat::Json, &mut Vec::new()).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let parsed =
            Fixture::try_parse_from(["fixture", "pi", "--root", root.to_str().unwrap()]).unwrap();
        run(parsed.args, CliOutputFormat::Json, &mut Vec::new()).unwrap();
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
            run_with_manifest(
                parsed.args,
                CliOutputFormat::Json,
                &mut output,
                Some(manifest.clone()),
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
