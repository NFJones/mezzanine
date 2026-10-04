//! Daemon-free bootstrap admission for release-qualified harness integrations.
//!
//! Candidate names are not certification. Unsupported versions fail before any
//! root/config/socket mutation. This boundary never takes user-authored manifest
//! JSON or executable templates; adapters must enter the compiled registry.

use super::{Args, CliOutputFormat, MezError, PathBuf, Result, Write};

/// Explicit installer intent; read-only planning is the default.
#[derive(Debug, Clone, Args)]
pub(super) struct BootstrapCliArgs {
    /// Harness whose compiled adapter should be consulted.
    #[arg(value_parser = ["claude", "codex", "gemini", "copilot", "opencode", "cursor"])]
    harness: String,
    /// Exact vendor release; no shell-based executable discovery is performed.
    #[arg(long)]
    vendor_version: Option<String>,
    /// Explicit existing user or project configuration root.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Inspect a plan without publishing artifacts.
    #[arg(long, conflicts_with_all = ["check", "apply", "uninstall", "recover"])]
    plan: bool,
    /// Check accepted ownership without publishing artifacts.
    #[arg(long, conflicts_with_all = ["apply", "uninstall", "recover"])]
    check: bool,
    /// Explicitly accept a certified installation/reconciliation plan.
    #[arg(long, conflicts_with_all = ["uninstall", "recover"])]
    apply: bool,
    /// Remove only unchanged, receipted adapter-owned artifacts.
    #[arg(long, conflicts_with = "recover")]
    uninstall: bool,
    /// Explicitly finish an already accepted publication journal.
    #[arg(long)]
    recover: bool,
}

/// Reports candidates honestly and refuses mutations before filesystem access.
/// Vendor-specific manifests are intentionally absent until independently certified.
pub(super) fn run<W: Write>(
    args: BootstrapCliArgs,
    format: CliOutputFormat,
    stdout: &mut W,
) -> Result<()> {
    let manifest = crate::integrations::bootstrap::certified_manifest(
        &args.harness,
        args.vendor_version.as_deref(),
    );
    run_with_manifest(args, format, stdout, manifest)
}

/// Executes only a compiled manifest matching the exact requested release.
/// Tests inject content-free fixtures, not a process-visible manifest interface.
fn run_with_manifest<W: Write>(
    args: BootstrapCliArgs,
    format: CliOutputFormat,
    stdout: &mut W,
    manifest: Option<crate::integrations::bootstrap::installer::Manifest>,
) -> Result<()> {
    if args.vendor_version.as_ref().is_some_and(|version| {
        version.is_empty() || version.len() > 128 || version.chars().any(char::is_control)
    }) {
        return Err(MezError::invalid_args(
            "bootstrap vendor version must be bounded inert text",
        ));
    }
    if let Some(manifest) = manifest {
        if manifest.harness != args.harness
            || args.vendor_version.as_deref() != Some(manifest.vendor_version.as_str())
        {
            return Err(MezError::invalid_args(
                "bootstrap requires an exact certified release",
            ));
        }
        let root = args
            .root
            .as_ref()
            .ok_or_else(|| MezError::invalid_args("bootstrap requires explicit existing --root"))?;
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
            "supported":true,"manifest_revision":manifest.revision,"changed_paths":changed_paths,"recovered":recovered,
            "guidance":"Installation is observational only. Complete the adapter's vendor review/restart; installation does not certify a live session or token coverage",
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
            "codex",
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
            "codex",
            "--root",
            "/missing/bootstrap/root",
            "--apply",
        ])
        .unwrap();
        assert!(run(parsed.args, CliOutputFormat::Json, &mut Vec::new()).is_err());
        assert!(Fixture::try_parse_from(["fixture", "codex", "--plan", "--apply"]).is_err());
    }
}
