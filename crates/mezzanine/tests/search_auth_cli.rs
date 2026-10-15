//! Real binary search-auth acceptance using synthetic stdin and a private temp
//! HOME/config root. Explicit file selection avoids native/DBus/OS keyring
//! operations; this exercises the existing file backend with fake credentials.

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::{Command, Output, Stdio},
};

/// Synthetic canary only; never a real provider credential.
const CANARY: &str = "synthetic-cli-search-S3cr3t+/=canary";

/// One isolated process root; no inherited environment or user configuration.
struct Fixture(PathBuf);

impl Fixture {
    /// Allocates a fresh task-created root and no credential backend service.
    fn new() -> Self {
        let root = fs::canonicalize("/tmp").unwrap().join(format!(
            "mez-search-cli-{}-{:032x}",
            std::process::id(),
            rand::random::<u128>()
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }

    /// Runs the built product with only named synthetic environment values.
    fn run(&self, args: &[&str], input: Option<&str>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mez"));
        command
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "dumb")
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        if let Some(input) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
        }
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains(CANARY), "synthetic secret leaked");
            assert!(
                !text.contains("c3ludGhldGljLWNsaS1zZWFyY2g"),
                "encoded synthetic secret leaked"
            );
        }
        output
    }
}

impl Drop for Fixture {
    /// Cleans only the fixture-created temp root, never a checkout or target.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The actual binary stores synthetic stdin locally, reports unverified status,
/// preserves another search identity, and never creates primary/model auth
/// configuration. Logout is exact-name scoped and help is credential-free.
#[test]
fn search_auth_binary_stdin_status_logout_and_help_are_local_only() {
    let f = Fixture::new();
    for args in [
        vec!["auth", "search", "--help"],
        vec!["auth", "search", "login", "--help"],
        vec!["auth", "search", "status", "--help"],
        vec!["auth", "search", "logout", "--help"],
    ] {
        let output = f.run(&args, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            fs::read_dir(&f.0).unwrap().next().is_none(),
            "help created configuration"
        );
    }
    for name in ["work", "other"] {
        let output = f.run(
            &[
                "auth",
                "search",
                "login",
                name,
                "--provider",
                "brave",
                "--stdin",
                "--credential-store",
                "file",
            ],
            Some(&format!("{CANARY}\n")),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("configured"));
        assert!(text.contains("false"));
        assert!(!text.contains("os-keyring:") && !text.contains("file:"));
    }
    let status = f.run(&["auth", "search", "status", "work"], None);
    assert!(status.status.success());
    assert!(String::from_utf8_lossy(&status.stdout).contains("present"));
    let logout = f.run(&["auth", "search", "logout", "work"], None);
    assert!(logout.status.success());
    let status = f.run(&["auth", "search", "status", "work"], None);
    assert!(String::from_utf8_lossy(&status.stdout).contains("missing"));
    let other = f.run(&["auth", "search", "status", "other"], None);
    assert!(String::from_utf8_lossy(&other.stdout).contains("present"));
    let mut queue = vec![f.0.clone()];
    while let Some(path) = queue.pop() {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                queue.push(entry.path());
            } else {
                assert!(
                    !["config.toml", "auth.toml", "mcp-auth.toml"]
                        .contains(&entry.file_name().to_str().unwrap())
                );
            }
        }
    }
}

/// Invalid inputs and missing explicit noninteractive sources are local errors,
/// never probes or successful authentication; CLI errors do not echo canaries.
#[test]
fn search_auth_binary_rejects_unsafe_or_unrepresentable_inputs() {
    let f = Fixture::new();
    for input in ["", " ", "key\r\nHeader: injected", "key\0"] {
        let result = f.run(
            &[
                "auth",
                "search",
                "login",
                "work",
                "--provider",
                "exa",
                "--stdin",
                "--credential-store",
                "file",
            ],
            Some(input),
        );
        assert!(!result.status.success());
    }
    let result = f.run(
        &[
            "auth",
            "search",
            "login",
            "work",
            "--provider",
            "exa",
            "--credential-store",
            "file",
        ],
        None,
    );
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("--stdin"));
    let result = f.run(
        &[
            "auth",
            "search",
            "login",
            "../openai",
            "--provider",
            "exa",
            "--stdin",
            "--credential-store",
            "file",
        ],
        Some(CANARY),
    );
    assert!(!result.status.success());
}

/// Actual binary diagnostics never echo attempted inline keys once the grammar
/// identifies auth search, in plain/JSON modes and with global option values.
/// These synthetic argv cases test rejection, not a supported secret input API.
#[test]
fn search_auth_binary_parse_errors_do_not_echo_attempted_inline_keys() {
    let f = Fixture::new();
    let token_flag = format!("--token={CANARY}");
    for globals in [
        vec![],
        vec!["--json"],
        vec!["-S", "/tmp/synthetic.sock"],
        vec!["--json", "-L", "synthetic"],
    ] {
        for tail in [
            vec![CANARY],
            vec![token_flag.as_str()],
            vec!["--provider", CANARY],
        ] {
            let mut args = globals.clone();
            args.extend(["auth", "search", "login", "work"]);
            if !tail.contains(&"--provider") {
                args.extend(["--provider", "brave"]);
            }
            args.extend(tail);
            let output = f.run(&args, None);
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("invalid auth search arguments")
            );
        }
    }
}
