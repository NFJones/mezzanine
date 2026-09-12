//! Remote identity, invitation, and trust persistence tests.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use iroh::SecretKey;
use rustix::fs::{FlockOperation, flock};
use secrecy::ExposeSecret;

use super::{RemoteEndpointIdentity, RemoteRoleCeiling, RemoteTrustStore};
use crate::control::RequestedRole;

/// Creates one isolated filesystem root for a remote security test.
fn test_root(label: &str) -> PathBuf {
    let nonce: u64 = rand::random();
    let root = std::env::temp_dir().join(format!(
        "mez-remote-security-{label}-{}-{nonce}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    root
}

/// Returns the retained endpoint identity lock for one test session root.
fn session_identity_lock_path(root: &Path) -> PathBuf {
    fs::read_dir(root.join("remote/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path()
        .join("endpoint.lock")
}

/// Verifies endpoint identity survives restart and excludes duplicate live use.
///
/// Iroh itself permits one secret key to back multiple live endpoints, so the
/// retained filesystem lock is the application invariant preventing duplicate
/// server identity use.
#[test]
fn endpoint_identity_persists_and_rejects_duplicate_live_use() {
    let root = test_root("identity");
    let identity = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap();
    let endpoint_id = identity.endpoint_id().to_string();

    let duplicate = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap_err();
    assert_eq!(duplicate.kind(), crate::error::MezErrorKind::Conflict);
    assert_eq!(identity.secret_key().public().to_string(), endpoint_id);

    drop(identity);
    let reloaded = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap();
    assert_eq!(reloaded.endpoint_id(), endpoint_id);

    let remote = fs::read_dir(root.join("remote/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::metadata(&remote).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for name in ["endpoint.key", "endpoint.lock"] {
        assert_eq!(
            fs::metadata(remote.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "{name}"
        );
    }

    drop(reloaded);
    let _ = fs::remove_dir_all(root);
}

/// Verifies acquisition outlasts a transient inherited identity lock.
///
/// `O_CLOEXEC` closes an inherited lock descriptor only at `exec`, so a child
/// forked by another thread can hold this lock for the fork-to-exec window even
/// though no live identity owns it. Acquisition must retry instead of failing
/// fast, and must still succeed once that transient holder releases.
///
/// A sibling test's forked child can keep that inherited descriptor open past
/// the production retry budget while the machine is loaded, so the scenario runs
/// in a bounded attempt loop: a conflict is tolerated only when acquisition
/// actually waited out the budget, and each such attempt is followed by a pause
/// long enough for the inherited descriptor to reach `exec` and release the
/// flock. The bounded retry is exactly what this test protects: acquisition must
/// outlast a transient holder instead of failing fast, and must not report the
/// documented live-use conflict for a holder no live identity owns.
#[test]
fn endpoint_identity_lock_acquisition_survives_transient_holder() {
    // Bounds attempts at the documented transient fork-inherited lock window.
    const MAX_ATTEMPTS: usize = 4;

    let root = test_root("transient-lock");
    let identity = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap();
    let endpoint_id = identity.endpoint_id().to_string();
    drop(identity);

    let lock_path = session_identity_lock_path(&root);
    for attempt in 1..=MAX_ATTEMPTS {
        let holder = super::store::open_private_lock(&lock_path).unwrap();
        flock(&holder, FlockOperation::NonBlockingLockExclusive).unwrap();
        // Keep each hold as short as the assertions allow: while it is open, any
        // sibling test thread that forks a PTY child inherits this descriptor and
        // keeps the flock alive until the child execs, so a longer hold widens a
        // window this attempt would then have to wait out inside the production
        // retry budget. The lock is already held before the acquisition starts,
        // so a few milliseconds still guarantee that acquisition observes a
        // holder.
        let releasing = thread::spawn(move || {
            thread::sleep(Duration::from_millis(5));
            drop(holder);
        });

        let started = Instant::now();
        let reacquired = RemoteEndpointIdentity::load_or_create(&root, "session-a");
        let waited = started.elapsed();

        match reacquired {
            Ok(reacquired) => {
                assert_eq!(reacquired.endpoint_id(), endpoint_id);
                assert!(
                    waited >= Duration::from_millis(1),
                    "acquisition returned after {waited:?} without waiting out the transient holder"
                );
                drop(reacquired);
                releasing.join().unwrap();
                let _ = fs::remove_dir_all(root);
                return;
            }
            Err(error) if error.kind() == crate::error::MezErrorKind::Conflict => {
                // A conflict is tolerable only once acquisition actually spent the
                // whole retry budget waiting on the holder.
                assert!(
                    waited >= super::store::IDENTITY_LOCK_RETRY_BUDGET,
                    "acquisition reported a conflict after {waited:?}, before the identity lock \
                     retry budget was exhausted"
                );
                releasing.join().unwrap();
                if attempt < MAX_ATTEMPTS {
                    // Let an inherited descriptor held by a sibling test's forked
                    // child pass `exec`, which finally releases the flock.
                    thread::sleep(Duration::from_millis(50));
                }
            }
            Err(error) => panic!("unexpected identity acquisition failure: {error:?}"),
        }
    }

    panic!(
        "endpoint identity acquisition still conflicted after {MAX_ATTEMPTS} attempts: a sibling \
         test's forked child held the inherited descriptor past the identity lock retry budget"
    );
}

/// Verifies a live holder still conflicts once the retry budget elapses.
///
/// The retry covers only a transient fork-inherited lock, so an identity that
/// stays held past the budget must still report the documented conflict with
/// unchanged kind and message, and must release cleanly afterwards.
#[test]
fn endpoint_identity_lock_conflicts_while_holder_stays_live() {
    let root = test_root("held-lock");
    let identity = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap();
    let endpoint_id = identity.endpoint_id().to_string();
    drop(identity);

    let holder = super::store::open_private_lock(&session_identity_lock_path(&root)).unwrap();
    flock(&holder, FlockOperation::NonBlockingLockExclusive).unwrap();

    let started = Instant::now();
    let conflict = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap_err();
    let waited = started.elapsed();
    assert_eq!(conflict.kind(), crate::error::MezErrorKind::Conflict);
    assert_eq!(
        conflict.message(),
        "Iroh endpoint identity is already in use by another live process"
    );
    assert!(
        waited >= super::store::IDENTITY_LOCK_RETRY_BUDGET,
        "conflict returned after {waited:?} before the retry budget elapsed"
    );

    drop(holder);
    let reacquired = RemoteEndpointIdentity::load_or_create(&root, "session-a").unwrap();
    assert_eq!(reacquired.endpoint_id(), endpoint_id);
    drop(reacquired);
    let _ = fs::remove_dir_all(root);
}

/// Verifies malformed or publicly readable endpoint key material fails closed.
#[test]
fn endpoint_identity_rejects_malformed_and_unsafe_key_files() {
    let malformed_root = test_root("malformed-key");
    let malformed_identity =
        RemoteEndpointIdentity::load_or_create(&malformed_root, "session-a").unwrap();
    drop(malformed_identity);
    let malformed_remote = fs::read_dir(malformed_root.join("remote/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(malformed_remote.join("endpoint.key"), [7u8; 3]).unwrap();
    fs::set_permissions(
        malformed_remote.join("endpoint.key"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();

    let malformed =
        RemoteEndpointIdentity::load_or_create(&malformed_root, "session-a").unwrap_err();
    assert_eq!(malformed.kind(), crate::error::MezErrorKind::InvalidState);

    let unsafe_root = test_root("unsafe-key");
    {
        let identity = RemoteEndpointIdentity::load_or_create(&unsafe_root, "session-a").unwrap();
        drop(identity);
    }
    fs::set_permissions(
        fs::read_dir(unsafe_root.join("remote/sessions"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("endpoint.key"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let unsafe_error =
        RemoteEndpointIdentity::load_or_create(&unsafe_root, "session-a").unwrap_err();
    assert_eq!(unsafe_error.kind(), crate::error::MezErrorKind::Forbidden);

    let _ = fs::remove_dir_all(malformed_root);
    let _ = fs::remove_dir_all(unsafe_root);
}

/// Verifies invitations are redacted, single-use, endpoint-bound, and role-limited.
#[test]
fn invitation_redemption_creates_role_limited_revocable_trust() {
    let root = test_root("invitation");
    let store = RemoteTrustStore::under_config_root(&root, "session-a").unwrap();
    let server_endpoint_id = SecretKey::generate().public().to_string();
    let client_endpoint_id = SecretKey::generate().public().to_string();
    let invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 1_000)
        .unwrap();
    let token = invitation.token.expose_secret().to_string();

    let debug = format!("{invitation:?}");
    assert!(!debug.contains(&token), "{debug}");
    let persisted = fs::read_to_string(store.directory().join("trust.json")).unwrap();
    assert!(!persisted.contains(&token), "{persisted}");

    let redemption = store
        .redeem_invitation(
            &invitation.token,
            &server_endpoint_id,
            &client_endpoint_id,
            "laptop",
            RequestedRole::Observer,
            1_100,
        )
        .unwrap();
    let record = redemption.record;
    let device_credential = redemption.device_credential;
    assert_eq!(record.role_ceiling, RemoteRoleCeiling::Observer);
    assert_eq!(record.endpoint_id, client_endpoint_id);

    let resumed = store
        .redeem_invitation(
            &invitation.token,
            &server_endpoint_id,
            &record.endpoint_id,
            "laptop",
            RequestedRole::Observer,
            1_101,
        )
        .unwrap();
    assert_eq!(resumed.record, record);
    assert_eq!(
        resumed.device_credential.expose_secret(),
        device_credential.expose_secret()
    );
    assert_eq!(store.list_records().unwrap().len(), 1);

    let principal = store
        .resolve_principal(
            &server_endpoint_id,
            &record.endpoint_id,
            &device_credential,
            RequestedRole::Observer,
            1_200,
        )
        .unwrap();
    assert_eq!(principal.trust_record_id, record.id);
    assert_eq!(principal.role_ceiling, RemoteRoleCeiling::Observer);
    assert_eq!(principal.requested_role, RequestedRole::Observer);

    let wrong_device_credential =
        secrecy::SecretString::from("wrong-device-credential".to_string());
    let wrong_proof = store
        .resolve_principal(
            &server_endpoint_id,
            &record.endpoint_id,
            &wrong_device_credential,
            RequestedRole::Observer,
            1_199,
        )
        .unwrap_err();
    assert_eq!(wrong_proof.kind(), crate::error::MezErrorKind::Forbidden);

    let elevation = store
        .resolve_principal(
            &server_endpoint_id,
            &record.endpoint_id,
            &device_credential,
            RequestedRole::Primary,
            1_201,
        )
        .unwrap_err();
    assert_eq!(elevation.kind(), crate::error::MezErrorKind::Forbidden);

    let renamed = store.rename_record(&record.id, "work laptop").unwrap();
    assert_eq!(renamed.label, "work laptop");
    let revoked = store
        .revoke_record(&record.id, Some("device retired"), 1_300)
        .unwrap();
    assert!(revoked.revoked());
    let revoked_error = store
        .resolve_principal(
            &server_endpoint_id,
            &record.endpoint_id,
            &device_credential,
            RequestedRole::Observer,
            1_301,
        )
        .unwrap_err();
    assert_eq!(revoked_error.kind(), crate::error::MezErrorKind::Forbidden);

    let records = store.list_records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].last_used_at_unix_seconds, Some(1_200));
    assert_eq!(
        fs::metadata(store.directory().join("trust.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let _ = fs::remove_dir_all(root);
}

/// Verifies a revoked endpoint can pair again and each credential resolves
/// only against the trust record that issued it.
///
/// Historical revocation must reject the original credential without
/// shadowing the later active record for the same persistent endpoint ID.
#[test]
fn revoked_endpoint_can_repair_with_new_credential() {
    let root = test_root("repaired-endpoint");
    let store = RemoteTrustStore::under_config_root(&root, "session-a").unwrap();
    let server_endpoint_id = SecretKey::generate().public().to_string();
    let client_endpoint_id = SecretKey::generate().public().to_string();
    let first_invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 1_000)
        .unwrap();
    let first = store
        .redeem_invitation(
            &first_invitation.token,
            &server_endpoint_id,
            &client_endpoint_id,
            "laptop",
            RequestedRole::Observer,
            1_001,
        )
        .unwrap();
    store
        .revoke_record(&first.record.id, Some("lost credential"), 1_002)
        .unwrap();

    let second_invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 1_003)
        .unwrap();
    let second = store
        .redeem_invitation(
            &second_invitation.token,
            &server_endpoint_id,
            &client_endpoint_id,
            "replacement laptop",
            RequestedRole::Observer,
            1_004,
        )
        .unwrap();

    let principal = store
        .resolve_principal(
            &server_endpoint_id,
            &client_endpoint_id,
            &second.device_credential,
            RequestedRole::Observer,
            1_005,
        )
        .unwrap();
    assert_eq!(principal.trust_record_id, second.record.id);
    let old = store
        .resolve_principal(
            &server_endpoint_id,
            &client_endpoint_id,
            &first.device_credential,
            RequestedRole::Observer,
            1_006,
        )
        .unwrap_err();
    assert_eq!(old.kind(), crate::error::MezErrorKind::Forbidden);
    assert_eq!(store.list_records().unwrap().len(), 2);

    let _ = fs::remove_dir_all(root);
}

/// Verifies a valid invitation supersedes an active trust record for the same endpoint.
///
/// Re-pairing is authenticated by the fresh invitation, so stale local credential
/// state must not require an administrator to revoke the previous record manually.
#[test]
fn active_endpoint_can_repair_and_supersede_previous_trust() {
    let root = test_root("active-repaired-endpoint");
    let store = RemoteTrustStore::under_config_root(&root, "session-a").unwrap();
    let server_endpoint_id = SecretKey::generate().public().to_string();
    let client_endpoint_id = SecretKey::generate().public().to_string();
    let first_invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 1_000)
        .unwrap();
    let first = store
        .redeem_invitation(
            &first_invitation.token,
            &server_endpoint_id,
            &client_endpoint_id,
            "laptop",
            RequestedRole::Observer,
            1_001,
        )
        .unwrap();

    let second_invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 1_002)
        .unwrap();
    let preparation = store
        .prepare_invitation(
            &second_invitation.token,
            &server_endpoint_id,
            &client_endpoint_id,
            "replacement laptop",
            RequestedRole::Observer,
            1_003,
        )
        .unwrap();
    let second = store.commit_invitation(preparation.clone(), 1_003).unwrap();

    let records = store.list_records().unwrap();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .any(|record| record.id == first.record.id && record.revoked())
    );
    assert!(
        records
            .iter()
            .any(|record| record.id == second.record.id && !record.revoked())
    );
    store.rollback_invitation_redemption(&second).unwrap();
    let records = store.list_records().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0], first.record);
    let restored = store
        .resolve_principal(
            &server_endpoint_id,
            &client_endpoint_id,
            &first.device_credential,
            RequestedRole::Observer,
            1_006,
        )
        .unwrap();
    assert_eq!(restored.trust_record_id, first.record.id);
    let recommitted = store.commit_invitation(preparation, 1_007).unwrap();
    assert_ne!(recommitted.record.id, first.record.id);
    let old = store
        .resolve_principal(
            &server_endpoint_id,
            &client_endpoint_id,
            &first.device_credential,
            RequestedRole::Observer,
            1_008,
        )
        .unwrap_err();
    assert_eq!(old.kind(), crate::error::MezErrorKind::Forbidden);
    let replacement = store
        .resolve_principal(
            &server_endpoint_id,
            &client_endpoint_id,
            &recommitted.device_credential,
            RequestedRole::Observer,
            1_009,
        )
        .unwrap();
    assert_eq!(replacement.trust_record_id, recommitted.record.id);

    let _ = fs::remove_dir_all(root);
}

/// Verifies expired and server-mismatched invitations fail without trust creation.
#[test]
fn invitation_rejects_expiry_and_server_mismatch() {
    let root = test_root("invitation-rejection");
    let store = RemoteTrustStore::under_config_root(&root, "session-a").unwrap();
    let server_endpoint_id = SecretKey::generate().public().to_string();
    let other_server_endpoint_id = SecretKey::generate().public().to_string();
    let client_endpoint_id = SecretKey::generate().public().to_string();
    let invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Primary, 30, 2_000)
        .unwrap();

    let mismatch = store
        .redeem_invitation(
            &invitation.token,
            &other_server_endpoint_id,
            &client_endpoint_id,
            "desktop",
            RequestedRole::Primary,
            2_001,
        )
        .unwrap_err();
    assert_eq!(mismatch.kind(), crate::error::MezErrorKind::Forbidden);

    let expired = store
        .redeem_invitation(
            &invitation.token,
            &server_endpoint_id,
            &client_endpoint_id,
            "desktop",
            RequestedRole::Primary,
            2_031,
        )
        .unwrap_err();
    assert_eq!(expired.kind(), crate::error::MezErrorKind::Forbidden);
    assert!(store.list_records().unwrap().is_empty());

    let _ = fs::remove_dir_all(root);
}

/// Verifies session hashing isolates state and unsafe symlink or oversized state fails closed.
#[test]
fn remote_trust_store_isolates_sessions_and_rejects_unsafe_state() {
    use std::os::unix::fs::symlink;

    let root = test_root("isolation-hardening");
    let first = RemoteTrustStore::under_config_root(&root, "session-a").unwrap();
    let traversal = RemoteTrustStore::under_config_root(&root, "../../session-a").unwrap();
    assert_ne!(first.directory(), traversal.directory());
    assert!(
        first
            .directory()
            .starts_with(root.join("remote").join("sessions"))
    );
    assert!(
        traversal
            .directory()
            .starts_with(root.join("remote").join("sessions"))
    );

    let server_endpoint_id = SecretKey::generate().public().to_string();
    first
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 1_000)
        .unwrap();
    assert!(traversal.list_records().unwrap().is_empty());

    let trust_path = first.directory().join("trust.json");
    fs::write(
        &trust_path,
        vec![b"x"[0]; super::store::MAX_TRUST_DATABASE_BYTES as usize + 1],
    )
    .unwrap();
    fs::set_permissions(&trust_path, fs::Permissions::from_mode(0o600)).unwrap();
    let oversized = first.list_records().unwrap_err();
    assert_eq!(oversized.kind(), crate::error::MezErrorKind::InvalidState);

    fs::remove_file(&trust_path).unwrap();
    let outside = root.join("outside.json");
    fs::write(
        &outside,
        b"{\"version\":1,\"records\":[],\"invitations\":[]}",
    )
    .unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&outside, &trust_path).unwrap();
    assert!(first.list_records().is_err());

    let symlink_root = test_root("directory-symlink");
    let outside_remote = test_root("outside-remote");
    fs::create_dir_all(&outside_remote).unwrap();
    fs::set_permissions(&outside_remote, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir_all(&symlink_root).unwrap();
    symlink(&outside_remote, symlink_root.join("remote")).unwrap();
    let symlink_store = RemoteTrustStore::under_config_root(&symlink_root, "session-a").unwrap();
    assert!(
        symlink_store
            .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 2_000,)
            .is_err()
    );

    let _ = fs::remove_dir_all(root);
    let _ = fs::remove_dir_all(symlink_root);
    let _ = fs::remove_dir_all(outside_remote);
}

/// Verifies concurrent claims let exactly one endpoint redeem an invitation.
#[test]
fn concurrent_invitation_redemption_selects_one_endpoint() {
    let root = test_root("concurrent-redemption");
    let store = Arc::new(RemoteTrustStore::under_config_root(&root, "session-a").unwrap());
    let server_endpoint_id = SecretKey::generate().public().to_string();
    let invitation = store
        .create_invitation(&server_endpoint_id, RemoteRoleCeiling::Observer, 600, 3_000)
        .unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let token = invitation.token.clone();
            let server_endpoint_id = server_endpoint_id.clone();
            thread::spawn(move || {
                let client_endpoint_id = SecretKey::generate().public().to_string();
                barrier.wait();
                store.redeem_invitation(
                    &token,
                    &server_endpoint_id,
                    &client_endpoint_id,
                    "concurrent client",
                    RequestedRole::Observer,
                    3_001,
                )
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(store.list_records().unwrap().len(), 1);

    let _ = fs::remove_dir_all(root);
}
