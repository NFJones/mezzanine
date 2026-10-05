//! Explicit launcher input and diagnostic safety qualification.
//!
//! Disposable fixtures use no remote credentials, broker startup or provider
//! work. Command construction is inspected separately from actual child reaping.

use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

/// Creates a private primary root and retains its elected launcher guard.
fn fixture() -> (std::path::PathBuf, CliEnv, StartupElection) {
    let home = std::env::temp_dir().join(format!("mez-launch-{:032x}", rand::random::<u128>()));
    let env = CliEnv {
        home: Some(home.clone()),
        ..Default::default()
    };
    let paths = env.config_paths().unwrap();
    paths.ensure_default_config().unwrap();
    let election = StartupElection::acquire(paths.root()).unwrap().unwrap();
    (home, env, election)
}

/// Fixed argv, explicit HOME/cwd and a cleared ambient environment must not
/// export frontend routing or credentials. Invalid paths reject before spawn.
#[test]
fn outbound_launcher_inputs_are_explicit_and_environment_isolated() {
    let (home, env, election) = fixture();
    let command = launch_command(Path::new("/bin/true"), &env, &election).unwrap();
    assert_eq!(command.get_program(), "/bin/true");
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        vec!["remote", "outbound-serve"]
    );
    assert_eq!(command.get_current_dir(), Some(home.as_path()));
    assert_eq!(
        command.get_envs().collect::<Vec<_>>(),
        vec![(std::ffi::OsStr::new("HOME"), Some(home.as_os_str()))]
    );
    assert!(launch_command(Path::new("mez"), &env, &election).is_err());
    let foreign = CliEnv {
        home: Some(home.join("other")),
        ..Default::default()
    };
    assert!(launch_command(Path::new("/bin/true"), &foreign, &election).is_err());
    drop(command);
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}

/// Diagnostic publication preserves authored unsafe entries rather than
/// following symlinks or changing permissions on permissive/hardlinked files.
#[test]
fn outbound_launcher_diagnostics_reject_unsafe_entries() {
    let (home, env, election) = fixture();
    let paths = env.config_paths().unwrap();
    let path = paths.root().join(DIAGNOSTIC_NAME);
    let target = paths.root().join("authored");
    std::fs::write(&target, b"preserve").unwrap();
    symlink(&target, &path).unwrap();
    assert!(launch_command(Path::new("/bin/true"), &env, &election).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"preserve");
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"existing diagnostics").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(launch_command(Path::new("/bin/true"), &env, &election).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"existing diagnostics");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&path, paths.root().join("alias.log")).unwrap();
    assert!(launch_command(Path::new("/bin/true"), &env, &election).is_err());
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}

/// A real child remains in the caller's exact handle and is deliberately
/// reaped. This verifies process transport, not actual broker readiness.
#[tokio::test]
async fn outbound_launcher_reaps_its_exact_child() {
    let (home, env, election) = fixture();
    let mut launched = LaunchedBroker::spawn(Path::new("/bin/true"), &env, &election).unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), launched.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success());
    assert!(launched.try_wait().unwrap().unwrap().success());
    let diagnostic = env.config_paths().unwrap().root().join(DIAGNOSTIC_NAME);
    assert_eq!(
        std::fs::metadata(diagnostic).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(launched);
    drop(election);
    std::fs::remove_dir_all(home).unwrap();
}
