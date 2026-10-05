//! Explicit launcher input and diagnostic safety qualification.
//!
//! Disposable fixtures use no remote credentials, broker startup or provider
//! work. Command construction is inspected separately from actual child reaping.

use super::*;
use std::os::unix::fs::{PermissionsExt, symlink};

/// Explicit released-binary fixture: the caller supplies a freshly built trusted
/// executable. Two authenticated readiness clients reuse one launched broker;
/// disposing them does not kill it. The test deliberately signals and reaps its
/// exact child, falling back to explicit termination on failure. No remote
/// sessions or provider work occur, and ordinary CLI routing is not qualified.
#[tokio::test]
#[ignore = "requires explicit MEZ_BROKER_EXECUTABLE for actual process qualification"]
async fn outbound_launcher_actual_broker_retains_child_and_reuses_readiness() {
    let executable = std::path::PathBuf::from(
        std::env::var_os("MEZ_BROKER_EXECUTABLE").expect("explicit broker executable required"),
    );
    assert!(executable.is_absolute());
    let (home, env, election) = fixture();
    drop(election);
    let mut child = None;
    let qualified = async {
        let first = connect_owned(
            &executable,
            &env,
            std::time::Duration::from_secs(10),
            &mut child,
        )
        .await?;
        let mut sibling_child = None;
        let second = connect_owned(
            Path::new("/nonexistent/unused-broker"),
            &env,
            std::time::Duration::from_secs(2),
            &mut sibling_child,
        )
        .await?;
        assert!(
            sibling_child.is_none(),
            "ready owner must not launch a sibling endpoint"
        );
        assert_ne!(first.handle()?, second.handle()?);
        drop(first);
        drop(second);
        let launched = child.as_mut().expect("elected caller retains child");
        assert!(launched.try_wait()?.is_none());
        let pid = launched
            .child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
            .expect("owned live child PID");
        rustix::process::kill_process(pid, rustix::process::Signal::TERM)
            .map_err(std::io::Error::from)?;
        let status = tokio::time::timeout(std::time::Duration::from_secs(10), launched.wait())
            .await
            .map_err(|_| MezError::invalid_state("fixture graceful broker shutdown timed out"))??;
        assert!(status.success());
        Ok::<(), MezError>(())
    }
    .await;
    if let Some(launched) = child.as_mut()
        && launched.try_wait().unwrap().is_none()
    {
        tokio::time::timeout(std::time::Duration::from_secs(5), launched.terminate())
            .await
            .expect("fixture child cleanup must finish")
            .unwrap();
    }
    qualified.unwrap();
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(crate::security::remote::RemoteClientIdentity::load_or_create(paths.root()).unwrap());
    drop(child);
    std::fs::remove_dir_all(home).unwrap();
}

/// An exited launcher with no published socket must remain caller-owned after
/// readiness timeout. The caller reaps it explicitly, and a second startup
/// attempt cannot silently replace the retained process evidence.
#[tokio::test]
async fn outbound_launcher_owned_startup_retains_failed_child() {
    let (home, env, election) = fixture();
    drop(election);
    let mut child = None;
    let result = connect_owned(
        Path::new("/bin/true"),
        &env,
        std::time::Duration::from_millis(200),
        &mut child,
    )
    .await;
    assert!(result.is_err());
    let launched = child
        .as_mut()
        .expect("spawned child must survive readiness failure");
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(5), launched.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(
        connect_owned(
            Path::new("/bin/true"),
            &env,
            std::time::Duration::from_secs(1),
            &mut child
        )
        .await
        .is_err()
    );
    assert!(
        child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .unwrap()
            .success()
    );
    drop(child);
    let paths = env.config_paths().unwrap();
    assert!(!paths.root().join("outbound.sock").exists());
    drop(StartupElection::acquire(paths.root()).unwrap().unwrap());
    std::fs::remove_dir_all(home).unwrap();
}

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
