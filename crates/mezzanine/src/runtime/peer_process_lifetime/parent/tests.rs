//! Deterministic native parent capture invariants, independent of scheduling.
use super::*;
use std::cell::Cell;

/// Separately exec'd parent/helper fixtures own only test sockets. The parent
/// spawns one helper and exits on its independent control byte; helper liveness
/// is bounded even if the controlling test fails before orderly teardown.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "self-executing native parent fixture; invoked by parent test"]
fn unix_parent_lifetime_process_fixture() {
    use std::io::{Read, Write};
    let Some(path) = std::env::var_os("MEZ_TEST_PARENT_SOCKET") else {
        return;
    };
    let role = std::env::var("MEZ_TEST_PARENT_ROLE").unwrap();
    let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(15)))
        .unwrap();
    stream.write_all(&[1]).unwrap();
    if role == "parent" {
        let helper = std::env::var_os("MEZ_TEST_HELPER_SOCKET").unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::peer_process_lifetime::parent::tests::unix_parent_lifetime_process_fixture", "--ignored"])
            .env("MEZ_TEST_PARENT_ROLE", "helper").env("MEZ_TEST_PARENT_SOCKET", helper)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().unwrap();
        // Reap the helper when it exits normally. A parent-first test deliberately
        // exits this whole fixture process while this wait thread remains blocked.
        let _reaper = std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    let mut stop = [0];
    stream.read_exact(&mut stop).unwrap();
    assert_eq!(stop, [2]);
}

/// Real separately exec'd socket origins distinguish parent lifetime from helper
/// lifetime in both death orders. No payload PID or inherited telemetry descriptor
/// supplies evidence, and the retained parent fd remains CLOEXEC/owned throughout.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn unix_parent_lifetime_real_death_orders_are_independent() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    /// Removes only the unique fixture directory, regardless of assertion exit.
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    /// Reaps only the directly spawned fixture parent; never signals a guessed PID.
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for parent_first in [false, true] {
        let directory = std::env::temp_dir().join(format!(
            "mez-parent-native-{}-{:x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir(&directory).unwrap();
        let _directory = Directory(directory.clone());
        let parent_path = directory.join("parent.sock");
        let helper_path = directory.join("helper.sock");
        let parent_listener = tokio::net::UnixListener::bind(&parent_path).unwrap();
        let helper_listener = tokio::net::UnixListener::bind(&helper_path).unwrap();
        let mut child = Child(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "runtime::peer_process_lifetime::parent::tests::unix_parent_lifetime_process_fixture", "--ignored"])
            .env("MEZ_TEST_PARENT_ROLE", "parent").env("MEZ_TEST_PARENT_SOCKET", &parent_path).env("MEZ_TEST_HELPER_SOCKET", &helper_path)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap());
        let budget = std::time::Duration::from_secs(10);
        let (mut parent_stream, _) = tokio::time::timeout(budget, parent_listener.accept())
            .await
            .unwrap()
            .unwrap();
        let (mut helper_stream, _) = tokio::time::timeout(budget, helper_listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut byte = [0];
        tokio::time::timeout(budget, parent_stream.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(byte, [1]);
        tokio::time::timeout(budget, helper_stream.read_exact(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(byte, [1]);
        let captured = capture_unix_origin(
            helper_stream.as_raw_fd(),
            crate::runtime::current_effective_uid(),
        );
        if let Err(error) = &captured
            && error.raw_os_error() == Some(libc::ENOPROTOOPT)
        {
            helper_stream.write_all(&[2]).await.unwrap();
            parent_stream.write_all(&[2]).await.unwrap();
            return;
        }
        let helper = captured.unwrap();
        let parent = helper.capture_parent().unwrap();
        assert_eq!(parent.identity.process_id, child.0.id());
        assert_eq!(parent.uid(), helper.uid());
        assert!(parent.is_live());
        let flags = unsafe { libc::fcntl(parent.lifetime.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        if !parent_first {
            helper_stream.write_all(&[2]).await.unwrap();
            tokio::time::timeout(budget, helper_stream.read_to_end(&mut Vec::new()))
                .await
                .unwrap()
                .unwrap();
            let deadline = std::time::Instant::now() + budget;
            while helper.is_live() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "closed helper socket did not imply bounded process exit"
                );
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert!(parent.is_live());
            assert_eq!(parent.reobserve().unwrap(), parent.identity);
        }
        parent_stream.write_all(&[2]).await.unwrap();
        let deadline = std::time::Instant::now() + budget;
        while child.0.try_wait().unwrap().is_none() {
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(!parent.is_live());
        assert!(parent.reobserve().is_err());
        if parent_first {
            assert!(
                helper.is_live(),
                "parent exit incorrectly proved helper death"
            );
            helper_stream.write_all(&[2]).await.unwrap();
            tokio::time::timeout(budget, helper_stream.read_to_end(&mut Vec::new()))
                .await
                .unwrap()
                .unwrap();
        }
    }
}

/// Exact same-user helper/parent observations accept; missing/replaced parent,
/// helper reparenting, cross-user evidence and native lifetime death reject.
#[test]
fn unix_parent_lifetime_capture_rejects_changed_native_evidence() {
    let helper = ProcessParentIdentity {
        process_id: 100,
        parent_process_id: 90,
        start_token: 1,
    };
    let parent = ProcessParentIdentity {
        process_id: 90,
        parent_process_id: 80,
        start_token: 2,
    };
    for mode in 0..8 {
        let helpers = Cell::new(0);
        let parents = Cell::new(0);
        let users = Cell::new(0);
        let result = capture_parent_with(
            1000,
            || {
                helpers.set(helpers.get() + 1);
                if mode == 1 && helpers.get() == 2 {
                    return Err(unavailable());
                }
                if mode == 2 && helpers.get() == 2 {
                    return Ok(ProcessParentIdentity {
                        parent_process_id: 80,
                        ..helper
                    });
                }
                Ok(helper)
            },
            |_| {
                parents.set(parents.get() + 1);
                if mode == 3 && parents.get() == 2 {
                    return Ok(ProcessParentIdentity {
                        start_token: 3,
                        ..parent
                    });
                }
                if mode == 4 {
                    return Err(unavailable());
                }
                Ok(parent)
            },
            |_| {
                users.set(users.get() + 1);
                Ok(if mode == 5 || (mode == 6 && users.get() == 2) {
                    2000
                } else {
                    1000
                })
            },
            |_| Ok(()),
            |_| {
                if mode == 7 {
                    Err(unavailable())
                } else {
                    Ok(())
                }
            },
            || false,
        );
        assert_eq!(result.is_ok(), mode == 0);
    }
}

/// Budget expiry during the final relationship read must reject even complete
/// evidence; root/init/self parent values cannot trigger any parent PID lookup.
#[test]
fn unix_parent_lifetime_rejects_late_final_read_and_invalid_parent() {
    let helper = ProcessParentIdentity {
        process_id: 100,
        parent_process_id: 90,
        start_token: 1,
    };
    let parent = ProcessParentIdentity {
        process_id: 90,
        parent_process_id: 80,
        start_token: 2,
    };
    let reads = Cell::new(0);
    let expired = Cell::new(false);
    let result = capture_parent_with(
        1000,
        || {
            reads.set(reads.get() + 1);
            if reads.get() == 2 {
                expired.set(true);
            }
            Ok(helper)
        },
        |_| Ok(parent),
        |_| Ok(1000),
        |_| Ok(()),
        |_| Ok(()),
        || expired.get(),
    );
    assert!(result.is_err());
    for ppid in [0, 1, 100] {
        let result = capture_parent_with(
            1000,
            || {
                Ok(ProcessParentIdentity {
                    parent_process_id: ppid,
                    ..helper
                })
            },
            |_| panic!("invalid parent triggered numeric lookup"),
            |_| Ok(1000),
            |_| Ok(()),
            |_| Ok(()),
            || false,
        );
        assert!(result.is_err());
    }
}

/// A dead helper avoids all parent lookup/open work. Failure after acquisition
/// drops the owned fence exactly once, so malformed/changed evidence cannot leak
/// a pidfd or silently retry numeric capture to select a replacement process.
#[test]
fn unix_parent_lifetime_capture_reclaims_failed_owned_fence() {
    struct Fence<'a>(&'a Cell<usize>);
    impl Drop for Fence<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }
    let helper = ProcessParentIdentity {
        process_id: 100,
        parent_process_id: 90,
        start_token: 1,
    };
    let parent = ProcessParentIdentity {
        process_id: 90,
        parent_process_id: 80,
        start_token: 2,
    };
    let drops = Cell::new(0);
    let result = capture_parent_with(
        1000,
        || Err(unavailable()),
        |_| panic!("dead helper caused parent lookup"),
        |_| Ok(1000),
        |_| Ok(Fence(&drops)),
        |_| Ok(()),
        || false,
    );
    assert!(result.is_err());
    assert_eq!(drops.get(), 0);
    let result = capture_parent_with(
        1000,
        || Ok(helper),
        |_| Ok(parent),
        |_| Ok(1000),
        |_| Ok(Fence(&drops)),
        |_| Err(unavailable()),
        || false,
    );
    assert!(result.is_err());
    assert_eq!(drops.get(), 1);
}
