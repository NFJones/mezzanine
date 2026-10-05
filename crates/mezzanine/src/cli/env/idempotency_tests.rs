//! CLI operation identity formatting and allocation regressions.
//!
//! A generated key belongs to one logical request, not a process lifetime.
//! Replay uses the retained value rather than calling the allocator again.

use super::*;

/// Generated keys distinguish independent durable creations under the same
/// principal. A retained request replays its original lease, while conflicting
/// reuse fails without allocating a third reservation.
#[test]
fn cli_idempotency_generated_keys_preserve_durable_replay() {
    use crate::storage::lease::{
        LeaseReservation, LeaseReservationRequest, RemoteSessionLeaseRepository,
    };
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "mez-cli-key-replay-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let repository = RemoteSessionLeaseRepository::new(root.clone());
    let request =
        |lease: &str, session: &str, key: &str, fingerprint: &str| LeaseReservationRequest {
            lease_id: lease.into(),
            session_id: session.into(),
            owner_principal_id: "device-1".into(),
            owner_live_session_limit: usize::MAX,
            name: None,
            default_for_owner: false,
            expires_at_unix_seconds: None,
            idempotency_key: key.into(),
            creation_fingerprint: fingerprint.into(),
            now_unix_seconds: 10,
        };
    let first_key = cli_idempotency_key("remote-session-create");
    let second_key = cli_idempotency_key("remote-session-create");
    assert_ne!(first_key, second_key);
    let first = request("lease-1", "$1", &first_key, "first");
    let created = repository.reserve_pending(first.clone()).unwrap();
    let replay = repository.reserve_pending(first).unwrap();
    assert!(matches!(replay, LeaseReservation::Replay(_)));
    assert_eq!(created.lease(), replay.lease());
    assert!(matches!(
        repository
            .reserve_pending(request("lease-2", "$2", &second_key, "second"))
            .unwrap(),
        LeaseReservation::Created(_)
    ));
    let conflict = repository
        .reserve_pending(request("lease-3", "$3", &first_key, "changed"))
        .unwrap_err();
    assert_eq!(conflict.kind(), crate::error::MezErrorKind::Conflict);
    assert_eq!(repository.list().unwrap().len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

/// Fixed nonces establish exact, bounded encoding independent of PID reuse or
/// wall-clock movement. Retaining the same operation/nonce preserves replay;
/// a different operation or nonce cannot collide with that request identity.
#[test]
fn cli_idempotency_nonce_encoding_preserves_exact_retry_identity() {
    let first = cli_idempotency_key_with_nonce("remote-session-create", 1);
    assert_eq!(
        first,
        "cli-remote-session-create-00000000000000000000000000000001"
    );
    assert_eq!(
        first,
        cli_idempotency_key_with_nonce("remote-session-create", 1)
    );
    assert_ne!(
        first,
        cli_idempotency_key_with_nonce("remote-session-create", 2)
    );
    assert_ne!(first, cli_idempotency_key_with_nonce("session-kill", 1));
    assert!(cli_idempotency_key_with_nonce("remote-session-create", u128::MAX).len() < 128);
}

/// Independent same-process operations allocate independent random identities,
/// while cloning a prepared key does not allocate a replacement for retries.
#[test]
fn cli_idempotency_allocates_independent_same_process_operations() {
    let keys = (0..256)
        .map(|_| cli_idempotency_key("remote-session-create"))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(keys.len(), 256);
    for key in keys {
        let retry = key.clone();
        assert_eq!(key, retry);
        let nonce = key.strip_prefix("cli-remote-session-create-").unwrap();
        assert_eq!(nonce.len(), 32);
        assert!(nonce.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}
