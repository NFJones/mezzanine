//! Explicit offline mod-source validation with an already-installed Claude CLI.
//!
//! This creates only a unique temporary plugin directory, never installs or
//! enables it and never sends a provider request. Callback facts still have no
//! Mezzanine transport/authority; validation qualifies source loading only.

/// The local vendor validator must accept literal classic-event bindings to the
/// importable no-Node observer library. Metadata validation is not certification
/// of runtime delivery, permissions, accounting or an installed integration.
#[test]
#[ignore = "explicit MEZ_TEST_CLAUDE_BINARY required; offline validation only"]
fn claude_observer_pure_callbacks_pass_offline_vendor_validation() {
    let executable = std::path::PathBuf::from(
        std::env::var_os("MEZ_TEST_CLAUDE_BINARY").expect("explicit installed Claude path"),
    );
    assert!(executable.is_absolute() && executable.is_file());
    let root = std::env::temp_dir().join(format!(
        "mez-claude-observer-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
    std::fs::create_dir(root.join("hooks")).unwrap();
    let _directory = Directory(root.clone());
    std::fs::write(
        root.join(".claude-plugin/plugin.json"),
        b"{\"name\":\"mez-offline-observer\",\"version\":\"0.0.1\"}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("hooks/hooks.json"),
        b"{\"modules\":[\"./register.mjs\"]}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("hooks/claude_observer.mjs"),
        include_bytes!("../src/integrations/bootstrap/claude_observer.mjs"),
    )
    .unwrap();
    std::fs::write(root.join("hooks/register.mjs"), b"import { projectClaudeEvent } from './claude_observer.mjs';\nexport function register(on) {\n const facts = [];\n on('classic.SessionStart', async ($, e, next) => { const fact = projectClaudeEvent('offline-session', 'SessionStart', e); const result = await next(e); if (fact) facts.push(fact); return result; });\n on('classic.UserPromptSubmit', async ($, e, next) => { const fact = projectClaudeEvent('offline-session', 'UserPromptSubmit', e); const result = await next(e); if (fact) facts.push(fact); return result; });\n on('classic.Notification', async ($, e, next) => { const fact = projectClaudeEvent('offline-session', 'Notification', e); const result = await next(e); if (fact) facts.push(fact); return result; });\n on('classic.Stop', async ($, e, next) => { const fact = projectClaudeEvent('offline-session', 'Stop', e); const result = await next(e); if (fact) facts.push(fact); return result; });\n on('classic.StopFailure', async ($, e, next) => { const fact = projectClaudeEvent('offline-session', 'StopFailure', e); const result = await next(e); if (fact) facts.push(fact); return result; });\n on('classic.SessionEnd', async ($, e, next) => { const fact = projectClaudeEvent('offline-session', 'SessionEnd', e); const result = await next(e); if (fact) facts.push(fact); return result; });\n}\n").unwrap();
    let result = std::process::Command::new(executable)
        .args(["plugin", "validate"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "offline vendor validation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout[..result.stdout.len().min(8192)]),
        String::from_utf8_lossy(&result.stderr[..result.stderr.len().min(8192)])
    );
}
