//! Assembled production installer qualification, not vendor runtime acceptance.
//! Every root and payload is fixture-owned; no vendor/provider is invoked.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Captures names, inode/mode/time and file bytes, excluding access time changed
/// by observation itself. New locks/journals/roots or content writes are visible.
type Observation = (PathBuf, u64, u32, i64, i64, Option<Vec<u8>>);

/// Returns bounded fixture observations for exact read-only comparisons.
fn snapshot(root: &Path) -> Vec<Observation> {
    fn walk(root: &Path, path: &Path, rows: &mut Vec<Observation>) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        rows.push((
            path.strip_prefix(root).unwrap().into(),
            metadata.ino(),
            metadata.mode(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.is_file().then(|| std::fs::read(path).unwrap()),
        ));
        if metadata.is_dir() {
            let mut children = std::fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                walk(root, &child, rows);
            }
        }
    }
    let mut rows = Vec::new();
    walk(root, root, &mut rows);
    rows
}

/// Bare production bootstrap must discover each documented default root, keep
/// whole-tree preview/check observation-only, install once/repeat unchanged,
/// repair a missing owned file and preserve unrelated data. This composes actual
/// compiled recipes with publication; it does not prove loaded hooks, process
/// association, accounting, custom-root loader acceptance or macOS execution.
#[test]
fn bootstrap_six_default_roots_preview_repeat_and_missing_file_repair() {
    for (harness, suffix, owned) in [
        (
            "claude",
            ".claude",
            "skills/mezzanine-telemetry/hooks/register.mjs",
        ),
        ("codex", ".codex", "hooks.json"),
        ("copilot", ".copilot", "hooks/mezzanine.json"),
        ("opencode", ".config/opencode", "plugins/mezzanine.js"),
        ("cursor", ".cursor", "hooks.json"),
        ("pi", ".pi/agent", "extensions/mezzanine/package.json"),
    ] {
        let home = std::env::temp_dir().join(format!(
            "mez-bootstrap-six-{harness}-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(home.join("unrelated.txt"), b"UNCHANGED USER DATA").unwrap();
        let vendor = home.join(suffix);
        let invoke = |flags: &[&str]| {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_mez"))
                .args(["--json", "bootstrap", harness])
                .args(flags)
                .env_clear()
                .env("HOME", &home)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            assert!(output.status.success(), "{harness}: {output:?}");
            assert!(output.stderr.is_empty());
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
        };
        let before = snapshot(&home);
        let preview = invoke(&["--dry-run"]);
        assert_eq!(preview["result"], "preview");
        assert_eq!(preview["scope_root"], vendor.to_str().unwrap());
        assert_eq!(snapshot(&home), before, "{harness} absent preview wrote");
        let installed = invoke(&[]);
        assert_eq!(installed["result"], "installed");
        assert_eq!(installed["runtime_verification"], "not-performed");
        assert_eq!(invoke(&[])["result"], "unchanged");
        std::fs::write(
            vendor.join("unrelated.json"),
            b"PRIVATE AUTHORED VENDOR DATA",
        )
        .unwrap();
        let before = snapshot(&home);
        assert_eq!(invoke(&["--check"])["result"], "checked");
        assert_eq!(snapshot(&home), before, "{harness} check wrote");
        assert_eq!(invoke(&["--dry-run"])["result"], "preview");
        assert_eq!(snapshot(&home), before, "{harness} installed preview wrote");
        assert_eq!(invoke(&["--uninstall", "--dry-run"])["result"], "preview");
        assert_eq!(snapshot(&home), before, "{harness} uninstall preview wrote");
        let owned = vendor.join(owned);
        let original = std::fs::read(&owned).unwrap();
        std::fs::remove_file(&owned).unwrap();
        assert_eq!(invoke(&[])["result"], "repaired");
        assert_eq!(std::fs::read(&owned).unwrap(), original);
        assert_eq!(
            std::fs::read(vendor.join("unrelated.json")).unwrap(),
            b"PRIVATE AUTHORED VENDOR DATA"
        );
        assert_eq!(
            std::fs::read(home.join("unrelated.txt")).unwrap(),
            b"UNCHANGED USER DATA"
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
