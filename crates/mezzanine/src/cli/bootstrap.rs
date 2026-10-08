//! Daemon-free bootstrap admission for compiled best-effort harness integrations.
//!
//! Candidate names are not certification. Versions do not gate compiled adapters.
//! This boundary never takes user-authored manifest
//! JSON or executable templates; adapters must enter the compiled registry.

use super::{Args, CliOutputFormat, MezError, PathBuf, Result, Write};

/// Explicit installer intent; read-only planning is the default.
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
    /// Inspect a plan without publishing artifacts.
    #[arg(long, conflicts_with_all = ["check", "apply", "uninstall", "recover"])]
    plan: bool,
    /// Check accepted ownership without publishing artifacts.
    #[arg(long, conflicts_with_all = ["apply", "uninstall", "recover"])]
    check: bool,
    /// Explicitly accept a compiled installation/reconciliation plan.
    #[arg(long, conflicts_with_all = ["uninstall", "recover"])]
    apply: bool,
    /// Remove only unchanged, receipted adapter-owned artifacts.
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
        let mut changed_paths = Vec::new();
        let operation = if args.recover {
            recovered = crate::integrations::bootstrap::installer::recover(root, &manifest)?;
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
            if args.apply || args.uninstall {
                plan.apply()?;
            }
            if args.uninstall {
                "uninstall"
            } else if args.apply {
                "apply"
            } else if args.check {
                "check"
            } else {
                "plan"
            }
        };
        return super::write_json_or_plain(stdout, format, &serde_json::json!({
            "harness":args.harness,"vendor_version":args.vendor_version,"operation":operation,
            "scope_root":root.to_string_lossy(),"root_source":selected.source,
            "supported":true,"manifest_revision":manifest.revision,"changed_paths":changed_paths,"recovered":recovered,
            "support":"best-effort",
            "guidance":"Installation is observational only. Use ordinary vendor commands and preserve vendor review/disabled policy. Automatic enrollment and accounting capabilities may still be unavailable; no Mez vendor-launch wrappers exist",
        }).to_string());
    }
    if args.apply || args.uninstall || args.recover {
        return Err(MezError::invalid_state(format!(
            "bootstrap {}: no release-qualified adapter manifest is installed; no files changed. Candidate support is not certification",
            args.harness
        )));
    }
    let output = serde_json::json!({
        "harness":args.harness, "vendor_version":args.vendor_version,
        "scope_root":args.root, "operation":if args.check {"check"} else {"plan"},
        "supported":false, "certification":"unavailable", "changed_paths":[],
        "lifecycle":"unavailable", "usage":"unavailable",
        "reason":"No release-qualified adapter manifest; no vendor configuration, credentials or hook trust changed",
    }).to_string();
    super::write_json_or_plain(stdout, format, &output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

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
            for intent in ["--check", "--apply", "--check"] {
                let parsed = Fixture::try_parse_from(["fixture", harness, intent]).unwrap();
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
                if intent == "--apply" {
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
            plan: false,
            check: true,
            apply: false,
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
        for intent in ["--check", "--apply", "--check"] {
            let parsed = Fixture::try_parse_from(vec![
                std::ffi::OsString::from("fixture"),
                "pi".into(),
                "--root".into(),
                root.clone().into_os_string(),
                intent.into(),
            ])
            .unwrap();
            let mut output = Vec::new();
            run(parsed.args, CliOutputFormat::Json, &mut output).unwrap();
            let output: serde_json::Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(output["scope_root"], root.to_string_lossy().as_ref());
            assert_eq!(output["root_source"], "explicit");
            if intent == "--apply" {
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
            for intent in ["--check", "--plan"] {
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
            Fixture::try_parse_from(["fixture", "pi", "--root", root.to_str().unwrap(), "--apply"])
                .unwrap();
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
            Fixture::try_parse_from(["fixture", "pi", "--root", root.to_str().unwrap(), "--apply"])
                .unwrap();
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
        for flag in ["--plan", "--check", "--apply", "--uninstall", "--recover"] {
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
            assert!(Fixture::try_parse_from(["fixture", harness, "--plan"]).is_ok());
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
            let parsed = Fixture::try_parse_from([
                "fixture",
                "codex",
                "--vendor-version",
                "fixture-only",
                "--root",
                root.to_str().unwrap(),
                flag,
            ])
            .unwrap();
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
            invoke("--plan")["changed_paths"].as_array().unwrap().len(),
            2
        );
        assert!(!root.join("owned.json").exists());
        invoke("--apply");
        assert_eq!(std::fs::read(root.join("owned.json")).unwrap(), b"{}\n");
        assert!(
            invoke("--check")["changed_paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            invoke("--apply")["changed_paths"]
                .as_array()
                .unwrap()
                .is_empty()
        );
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
            "--plan",
        ])
        .unwrap();
        let mut output = Vec::new();
        run(parsed.args, CliOutputFormat::Json, &mut output).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["supported"], false);
        let parsed = Fixture::try_parse_from([
            "fixture",
            "claude",
            "--root",
            "/missing/bootstrap/root",
            "--apply",
        ])
        .unwrap();
        assert!(run(parsed.args, CliOutputFormat::Json, &mut Vec::new()).is_err());
        assert!(Fixture::try_parse_from(["fixture", "codex", "--plan", "--apply"]).is_err());
    }
}
