//! Deterministic exact-conversation receipt admission/retirement regressions.
//!
//! Fixtures use temporary stores and per-handle channel barriers, never physical
//! shell processes or providers. Every competing settlement finishes after the
//! reader enumerates paths and before it can open those paths.

use super::*;
use std::sync::mpsc;
use std::time::Duration;

/// A's lock cannot protect B's receipt pathname. Retire B after A enumerates
/// it, then require A's unrelated valid admission to succeed without a retry.
/// This forces the historical stat/open NotFound rather than hoping for a race.
#[test]
fn transcript_receipt_admission_survives_other_conversation_retirement() {
    let root = temp_root("receipt-enumeration-retirement");
    let store = AgentTranscriptStore::new(root.clone());
    let a = entry("conversation-a", 1, TranscriptRole::User);
    let b = entry("conversation-b", 1, TranscriptRole::User);
    store
        .accept_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    store.append(&b).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let admission = thread::spawn({
        let store = store.clone();
        let a = a.clone();
        move || store.accept_append_receipt(&[a], 1)
    });
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // This separate handle has B's exact lock, not the paused A lock.
    AgentTranscriptStore::new(root.clone())
        .settle_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    resume_tx.send(()).unwrap();
    admission.join().unwrap().unwrap();
    assert_eq!(
        store.pending_append_receipts().unwrap(),
        vec![vec![a.clone()]]
    );
    store.recover_append_receipts().unwrap();
    assert_eq!(store.inspect(&a.conversation_id).unwrap(), vec![a]);
    assert_eq!(store.inspect(&b.conversation_id).unwrap(), vec![b]);
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Recovery's initial discovery owns no conversation yet. B may legitimately
/// settle before recovery acquires B's lock; a fresh locked enumeration must
/// omit that retired receipt while still recovering A's pending exact rows.
#[test]
fn transcript_receipt_recovery_survives_retirement_before_lock() {
    let root = temp_root("receipt-recovery-retirement");
    let store = AgentTranscriptStore::new(root.clone());
    let a = entry("conversation-a", 1, TranscriptRole::User);
    let b = entry("conversation-b", 1, TranscriptRole::User);
    for row in [&a, &b] {
        store
            .accept_append_receipt(std::slice::from_ref(row), 1)
            .unwrap();
    }
    store.append(&b).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let recovery = thread::spawn({
        let store = store.clone();
        move || store.recover_append_receipts()
    });
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    AgentTranscriptStore::new(root.clone())
        .settle_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    resume_tx.send(()).unwrap();
    recovery.join().unwrap().unwrap();
    assert_eq!(store.inspect(&a.conversation_id).unwrap(), vec![a]);
    assert_eq!(store.inspect(&b.conversation_id).unwrap(), vec![b]);
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// A selected receipt disappearing while A's lock is held is not cooperative
/// retirement. Simulated external loss must still fail admission; this guards
/// against fixing the unrelated B race by broadly suppressing NotFound errors.
#[test]
fn transcript_receipt_admission_rejects_selected_path_disappearance() {
    let root = temp_root("receipt-selected-disappearance");
    let store = AgentTranscriptStore::new(root.clone());
    let first = entry("conversation-a", 1, TranscriptRole::User);
    let second = entry("conversation-a", 2, TranscriptRole::Assistant);
    store.accept_append_receipt(&[first], 1).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let admission = thread::spawn({
        let store = store.clone();
        move || store.accept_append_receipt(&[second], 2)
    });
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    fs::remove_file(
        root.join(".append-receipts/conversation-a-00000000000000000001-00000000000000000001.json"),
    )
    .unwrap();
    resume_tx.send(()).unwrap();
    let error = admission.join().unwrap().unwrap_err();
    assert!(error.message().contains("No such file or directory"));
    assert!(store.pending_append_receipts().unwrap().is_empty());
    assert!(!store.transcript_path("conversation-a").unwrap().exists());
    fs::remove_dir_all(root).unwrap();
}

/// The scoped scan and receipt installation remain in A's exact lock domain.
/// A second open cannot acquire that lock at the paused enumeration boundary;
/// after release a matching duplicate settles only its own generation.
#[test]
fn transcript_receipt_admission_retains_same_conversation_lock() {
    let root = temp_root("receipt-same-conversation-lock");
    let store = AgentTranscriptStore::new(root.clone());
    let row = entry("conversation-a", 1, TranscriptRole::User);
    store
        .accept_append_receipt(std::slice::from_ref(&row), 1)
        .unwrap();
    store.append(&row).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let admission = thread::spawn({
        let store = store.clone();
        let row = row.clone();
        move || store.accept_append_receipt(&[row], 2)
    });
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(".conversation-locks/conversation-a.lock"))
        .unwrap();
    let blocked = rustix::fs::flock(
        &contender,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .map_err(std::io::Error::from)
    .unwrap_err();
    assert_eq!(blocked.kind(), std::io::ErrorKind::WouldBlock);
    resume_tx.send(()).unwrap();
    admission.join().unwrap().unwrap();
    store
        .settle_append_receipt(std::slice::from_ref(&row), 1)
        .unwrap();
    assert_eq!(
        store.pending_append_receipts().unwrap(),
        vec![vec![row.clone()]]
    );
    store
        .settle_append_receipt(std::slice::from_ref(&row), 2)
        .unwrap();
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Canonical B filenames route damage only to B's owner, not A or an A-prefix
/// match. B admission and global recovery still reject corrupt JSON, mismatched
/// identity/digest, directories and symlinks before replaying A's healthy rows.
#[cfg(unix)]
#[test]
fn transcript_receipt_scope_isolates_foreign_damage_but_recovery_fails_closed() {
    for damage in ["json", "identity", "digest", "directory", "symlink"] {
        let root = temp_root(&format!("receipt-damage-{damage}"));
        let store = AgentTranscriptStore::new(root.clone());
        let a = entry("a", 1, TranscriptRole::User);
        let b = entry("a-extra", 1, TranscriptRole::User);
        store
            .accept_append_receipt(std::slice::from_ref(&b), 1)
            .unwrap();
        let path =
            root.join(".append-receipts/a-extra-00000000000000000001-00000000000000000001.json");
        match damage {
            "json" => fs::write(&path, b"{broken").unwrap(),
            "identity" | "digest" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                let field = if damage == "identity" {
                    "conversation_id"
                } else {
                    "digest"
                };
                value[field] = "a".into();
                fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
            "directory" => {
                fs::remove_file(&path).unwrap();
                fs::create_dir(&path).unwrap();
            }
            "symlink" => {
                let outside = root.join("redirected.json");
                fs::rename(&path, &outside).unwrap();
                std::os::unix::fs::symlink(&outside, &path).unwrap();
            }
            _ => unreachable!(),
        }
        store
            .accept_append_receipt(std::slice::from_ref(&a), 1)
            .unwrap();
        assert!(store.accept_append_receipt(&[b], 2).is_err(), "{damage}");
        assert!(store.recover_append_receipts().is_err(), "{damage}");
        assert!(!store.transcript_path("a").unwrap().exists(), "{damage}");
        assert!(path.exists(), "{damage}");
        fs::remove_dir_all(root).unwrap();
    }
}

/// Unknown names cannot be assigned to a safe other-conversation lock owner.
/// Reject unknown extensions, noncanonical/zero/overflow numeric fields and
/// malformed staging names in admission as well as recovery without mutation.
#[test]
fn transcript_receipt_admission_rejects_unknown_filename_identity() {
    for name in [
        "unknown.json",
        "unknown",
        "unknown.json.tmp",
        "other-1-00000000000000000001.json",
        "other-00000000000000000000-00000000000000000001.json",
        "other-00000000000000000001-00000000000000000000.json",
        "other-18446744073709551616-00000000000000000001.json",
        "other-00000000000000000001-18446744073709551616.json",
        "other-0000000000000000000x-00000000000000000001.json",
        "other-00000000000000000001-0000000000000000000１.json",
    ] {
        let root = temp_root("receipt-unknown-name");
        let store = AgentTranscriptStore::new(root.clone());
        let directory = root.join(".append-receipts");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(name);
        fs::write(&path, b"not authority").unwrap();
        let a = entry("a", 1, TranscriptRole::User);
        assert!(store.accept_append_receipt(&[a], 1).is_err(), "{name}");
        assert!(store.recover_append_receipts().is_err(), "{name}");
        assert_eq!(fs::read(&path).unwrap(), b"not authority");
        assert!(!store.transcript_path("a").unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }
}

/// Parsing from the right must preserve all valid hyphen/underscore CID forms,
/// numeric-looking suffixes and the full u64 generation range, not confuse a
/// CID prefix for ownership or narrow names previously emitted by admission.
#[test]
fn transcript_receipt_admission_preserves_exact_filename_identity() {
    let root = temp_root("receipt-filename-identity");
    let store = AgentTranscriptStore::new(root.clone());
    for id in ["a", "a-extra", "a-", "-", "_", "a-00000000000000000001"] {
        let row = entry(id, 1, TranscriptRole::User);
        store
            .accept_append_receipt(std::slice::from_ref(&row), u64::MAX)
            .unwrap();
        store
            .accept_append_receipt(std::slice::from_ref(&row), u64::MAX)
            .unwrap();
    }
    assert_eq!(store.pending_append_receipts().unwrap().len(), 6);
    store.recover_append_receipts().unwrap();
    assert!(store.pending_append_receipts().unwrap().is_empty());
    fs::remove_dir_all(root).unwrap();
}

/// Non-UTF8 receipt names are unknown identities, never lossy routing hints.
/// Admission and global recovery must both refuse them and leave bytes intact.
#[cfg(unix)]
#[test]
fn transcript_receipt_admission_rejects_non_utf8_filename() {
    use std::os::unix::ffi::OsStringExt;
    let root = temp_root("receipt-non-utf8-name");
    let store = AgentTranscriptStore::new(root.clone());
    fs::create_dir_all(root.join(".append-receipts")).unwrap();
    let path = root
        .join(".append-receipts")
        .join(std::ffi::OsString::from_vec(vec![
            0xff, b'.', b'j', b's', b'o', b'n',
        ]));
    fs::write(&path, b"unknown").unwrap();
    assert!(
        store
            .accept_append_receipt(&[entry("a", 1, TranscriptRole::User)], 1)
            .is_err()
    );
    assert!(store.recover_append_receipts().is_err());
    assert_eq!(fs::read(path).unwrap(), b"unknown");
    fs::remove_dir_all(root).unwrap();
}

/// Fork already owns both source and target locks. Retiring B after that
/// fence enumerates paths must not make a safe unrelated fork fail, while
/// retained source or target receipts must still prohibit fork publication.
#[test]
fn transcript_receipt_fork_fence_uses_only_locked_identities() {
    let root = temp_root("receipt-fork-retirement");
    let store = AgentTranscriptStore::new(root.clone());
    let a = entry("conversation-a", 1, TranscriptRole::User);
    let b = entry("conversation-b", 1, TranscriptRole::User);
    store.append(&a).unwrap();
    store
        .accept_append_receipt(std::slice::from_ref(&b), 1)
        .unwrap();
    store.append(&b).unwrap();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    store.gate_next_receipt_enumeration_for_tests(observed_tx, resume_rx);
    let fork = thread::spawn({
        let store = store.clone();
        move || store.fork("conversation-a", "fork-target", 2)
    });
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    AgentTranscriptStore::new(root.clone())
        .settle_append_receipt(&[b], 1)
        .unwrap();
    resume_tx.send(()).unwrap();
    fork.join().unwrap().unwrap();
    assert_eq!(store.inspect("fork-target").unwrap().len(), 1);
    store
        .accept_append_receipt(std::slice::from_ref(&a), 1)
        .unwrap();
    assert!(store.fork("conversation-a", "blocked-source", 3).is_err());
    store.settle_append_receipt(&[a], 1).unwrap();
    let reserved = entry("reserved-target", 1, TranscriptRole::User);
    store.accept_append_receipt(&[reserved], 1).unwrap();
    assert!(store.fork("conversation-a", "reserved-target", 3).is_err());
    assert!(!store.transcript_path("blocked-source").unwrap().exists());
    assert!(!store.transcript_path("reserved-target").unwrap().exists());
    fs::remove_dir_all(root).unwrap();
}
