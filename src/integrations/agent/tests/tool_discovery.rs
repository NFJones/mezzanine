//! Agent tests for tool discovery behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies failed pane-shell probes are projected as product runtime errors.
///
/// Tool discovery is lower-owned, while this test remains with the concrete
/// pane executor adapter and its product error mapping.
fn tool_discovery_reports_shell_failures() {
    let signature = test_env_signature("host", "user", "/bin/sh", "/repo");
    let mut cache = ToolDiscoveryCache::default();
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(2),
            signal: None,
            stdout: String::new(),
            stderr: "no shell\n".to_string(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let error = discover_tools_through_pane_shell(
        &mut cache,
        signature,
        &turn(),
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidState);
    assert!(error.message().contains("tool discovery failed"));
}

#[test]
/// Verifies tool discovery runs through pane shell and caches by signature.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn tool_discovery_runs_through_pane_shell_and_caches_by_signature() {
    let signature = test_env_signature("host", "user", "/bin/sh", "/repo");
    let mut cache = ToolDiscoveryCache::default();
    let mut executor = FakePaneShellExecutor {
        output: Some(ShellExecutionOutput {
            exit_code: Some(0),
            signal: None,
            stdout:
                "tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n\
                 tool\tgrep\t1\t/usr/bin/grep\tGNU grep 3.11\tcommand -v grep\t0\t/usr/bin/grep --version\t0\t1714500000\n\
                 tool\tpython\t1\t/usr/bin/python3\tPython 3.12.3\tcommand -v python3 || command -v python\t0\t/usr/bin/python3 --version\t0\t1714500000\n\
                 tool\trg\t1\t/usr/bin/rg\tripgrep 14.1.1\tcommand -v rg\t0\t/usr/bin/rg --version\t0\t1714500000\n\
                 tool\tfd\t0\t\t\tcommand -v fd\t1\t\t\t1714500000\n"
                    .to_string(),
            stderr: String::new(),
            timed_out: false,
            interrupted: false,
            transport_diagnostics: Default::default(),
        }),
        ..FakePaneShellExecutor::default()
    };

    let first = discover_tools_through_pane_shell(
        &mut cache,
        signature.clone(),
        &turn(),
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();
    let second = discover_tools_through_pane_shell(
        &mut cache,
        signature,
        &turn(),
        marker(),
        Path::new("/bin/sh"),
        &mut executor,
    )
    .unwrap();

    assert!(first.rg);
    assert!(second.rg);
    let rg = first.tools.get("rg").unwrap();
    assert_eq!(rg.path.as_deref(), Some("/usr/bin/rg"));
    assert_eq!(rg.version.as_deref(), Some("ripgrep 14.1.1"));
    assert_eq!(rg.lookup_command, "command -v rg");
    assert_eq!(rg.lookup_exit_status, Some(0));
    assert_eq!(rg.version_exit_status, Some(0));
    assert_eq!(rg.discovered_at_unix_seconds, Some(1714500000));
    assert_eq!(executor.requests.len(), 1);
    assert_eq!(executor.requests[0].action_id, "tool-discovery:turn-1");
    assert_eq!(
        executor.requests[0].timeout_ms,
        Some(DEFAULT_TOOL_DISCOVERY_TIMEOUT_MS)
    );
    assert!(
        executor.requests[0]
            .transaction
            .command
            .contains("command -v")
    );
}
