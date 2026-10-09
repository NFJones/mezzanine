//! Active binding checkpoint concurrency, legacy import and recovery tests.
//!
//! All files are synthetic and isolated. Legacy writers deliberately do not
//! participate in new locking, matching old/new daemon coexistence.

use super::*;
use std::io::{Read, Seek, SeekFrom};

/// Namespace creation and an AlreadyExists retry both require the parent sync
/// barrier before any empty/nonempty checkpoint can be acknowledged. Failure
/// after mkdir leaves no authoritative checkpoint; exact retry remains possible.
#[test]
fn active_metadata_namespace_parent_sync_is_required_on_creation_and_retry() {
    let root = temp_root("active-metadata-parent-sync");
    let store = AgentTranscriptStore::new(root.clone());
    for _ in 0..2 {
        store.fail_active_metadata_phase_for_tests(6);
        assert!(store.save_agent_session_metadata("$scope", &[]).is_err());
        assert!(root.join(".active-agent-session-metadata-v1").is_dir());
        assert!(
            !store
                .agent_session_metadata_checkpoint_file("$scope")
                .exists()
        );
    }
    store.save_agent_session_metadata("$scope", &[]).unwrap();
    assert!(
        AgentTranscriptStore::new(root)
            .load_agent_session_metadata("$scope")
            .unwrap()
            .is_empty()
    );
}

/// Public direct-save and lazy-import boundaries must report that authority may
/// already have changed after rename, not expose a generic precommit-looking
/// I/O error. Reopening proves the new whole checkpoint actually landed.
#[test]
fn active_metadata_public_import_and_save_report_postrename_uncertainty() {
    for phase in [3, 4] {
        let root = temp_root("active-metadata-public-uncertainty");
        let store = AgentTranscriptStore::new(root.clone());
        let record = agent_session_metadata("$scope", "new-conversation");
        store.save_agent_session_metadata("$scope", &[]).unwrap();
        store.fail_active_metadata_phase_for_tests(phase);
        let error = store
            .save_agent_session_metadata("$scope", std::slice::from_ref(&record))
            .unwrap_err();
        assert!(
            error
                .message()
                .contains("published but durability is uncertain"),
            "{error}"
        );
        assert_eq!(
            AgentTranscriptStore::new(root)
                .load_agent_session_metadata("$scope")
                .unwrap(),
            vec![record.clone()]
        );
        let root = temp_root("active-metadata-import-uncertainty");
        fs::create_dir_all(&root).unwrap();
        let store = AgentTranscriptStore::new(root.clone());
        let legacy = format!("{}\n", encode_agent_session_metadata(&record).unwrap());
        fs::write(store.agent_session_metadata_file(), &legacy).unwrap();
        store.fail_active_metadata_phase_for_tests(phase);
        let error = store.load_agent_session_metadata("$scope").unwrap_err();
        assert!(
            error
                .message()
                .contains("published but durability is uncertain"),
            "{error}"
        );
        assert_eq!(
            AgentTranscriptStore::new(root)
                .load_agent_session_metadata("$scope")
                .unwrap(),
            vec![record]
        );
        assert_eq!(
            fs::read(store.agent_session_metadata_file()).unwrap(),
            legacy.as_bytes()
        );
    }
}

/// A recovery marker cannot outlive its exact preserved source unnoticed.
/// Missing or modified backup evidence fails closed without changing either
/// the original legacy file or the authoritative imported binding file.
#[test]
fn active_metadata_missing_or_changed_backup_is_not_claimed_recovered() {
    let root = temp_root("active-metadata-backup-proof");
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let record = agent_session_metadata("$old", "old-conversation");
    let original = format!("{}\n]\n", encode_agent_session_metadata(&record).unwrap());
    fs::write(store.agent_session_metadata_file(), &original).unwrap();
    store.load_agent_session_metadata("$old").unwrap();
    let checkpoint = store.agent_session_metadata_checkpoint_file("$old");
    let authoritative = fs::read(&checkpoint).unwrap();
    let backup = fs::read_dir(root.join(".active-agent-session-metadata-v1"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "tsv"))
        .unwrap();
    fs::write(&backup, b"altered backup").unwrap();
    assert!(
        store
            .load_agent_session_metadata("$old")
            .unwrap_err()
            .message()
            .contains("backup bytes changed")
    );
    fs::remove_file(backup).unwrap();
    assert!(
        store
            .load_agent_session_metadata("$old")
            .unwrap_err()
            .message()
            .contains("backup is missing")
    );
    assert_eq!(fs::read(checkpoint).unwrap(), authoritative);
    assert_eq!(
        fs::read(store.agent_session_metadata_file()).unwrap(),
        original.as_bytes()
    );
}

/// An independently opened lock holder must produce a finite admission failure
/// rather than a hung startup/worker. Once that owner releases, exact snapshot
/// replacement is possible and the old binding remained authoritative meanwhile.
#[test]
fn active_metadata_lock_contention_is_bounded_and_retryable() {
    use std::time::{Duration, Instant};
    let root = temp_root("active-metadata-lock-contention");
    let store = AgentTranscriptStore::new(root.clone());
    let old = agent_session_metadata("$scope", "old-conversation");
    store
        .save_agent_session_metadata("$scope", std::slice::from_ref(&old))
        .unwrap();
    let lock = store.lock_agent_session_metadata("$scope").unwrap();
    let contender = thread::spawn(move || {
        let independent = AgentTranscriptStore::new(root);
        let start = Instant::now();
        let error = independent
            .save_agent_session_metadata("$scope", &[])
            .unwrap_err();
        assert!(error.message().contains("lock timed out"));
        assert!(
            start.elapsed() >= Duration::from_secs(2) && start.elapsed() < Duration::from_secs(10)
        );
    });
    contender.join().unwrap();
    drop(lock);
    assert_eq!(
        store.load_agent_session_metadata("$scope").unwrap(),
        vec![old]
    );
    store.save_agent_session_metadata("$scope", &[]).unwrap();
    assert!(
        store
            .load_agent_session_metadata("$scope")
            .unwrap()
            .is_empty()
    );
}

/// Two independently running processes update disjoint sessions or compete for
/// one replacement checkpoint. Whole snapshots must remain decodable, foreign
/// sessions must not be lost, and no fixed legacy staging inode may be used.
#[test]
fn active_metadata_cross_process_writers_publish_whole_isolated_snapshots() {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    for shared in [false, true] {
        let root = temp_root("active-metadata-processes");
        fs::create_dir_all(&root).unwrap();
        let mut children = Vec::new();
        for index in 0..2 {
            children.push(Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "storage::transcript::tests::active_metadata::active_metadata_process_writer"])
                .env("MEZ_TEST_ACTIVE_METADATA_CHILD", "1")
                .env("MEZ_TEST_ACTIVE_METADATA_ROOT", &root)
                .env("MEZ_TEST_ACTIVE_METADATA_INDEX", index.to_string())
                .env("MEZ_TEST_ACTIVE_METADATA_SHARED", shared.to_string())
                .stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap());
        }
        let start = Instant::now();
        while !(root.join("ready-0").exists() && root.join("ready-1").exists()) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "metadata child did not reach the barrier"
            );
            thread::sleep(Duration::from_millis(5));
        }
        fs::write(root.join("go"), b"start").unwrap();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let store = AgentTranscriptStore::new(root.clone());
        for index in 0..2 {
            let id = if shared {
                "$shared".to_string()
            } else {
                format!("$process-{index}")
            };
            let records = store.load_agent_session_metadata(&id).unwrap();
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].transcript_entries, 19);
            if !shared {
                assert_eq!(records[0].conversation_id, format!("process-{index}"));
            }
            assert!(
                records[0].directive.as_ref().unwrap().len() == 2000
                    || records[0].directive.as_ref().unwrap().len() == 2002
            );
        }
        assert!(!root.join("active-agent-sessions.tsv").exists());
        assert!(!root.join("active-agent-sessions.tmp").exists());
        assert!(
            !fs::read_dir(root.join(".active-agent-session-metadata-v1"))
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".stage-"))
        );
    }
}

/// Child fixture for the real cross-process regression. Ordinary test runs are
/// inert; only the parent's explicit isolated-root invocation crosses a barrier.
#[test]
fn active_metadata_process_writer() {
    use std::time::{Duration, Instant};
    if std::env::var("MEZ_TEST_ACTIVE_METADATA_CHILD").as_deref() != Ok("1") {
        return;
    }
    let root = PathBuf::from(std::env::var_os("MEZ_TEST_ACTIVE_METADATA_ROOT").unwrap());
    assert!(
        root.canonicalize()
            .unwrap()
            .starts_with(std::env::temp_dir().canonicalize().unwrap())
    );
    let index: usize = std::env::var("MEZ_TEST_ACTIVE_METADATA_INDEX")
        .unwrap()
        .parse()
        .unwrap();
    let shared = std::env::var("MEZ_TEST_ACTIVE_METADATA_SHARED").unwrap() == "true";
    let store = AgentTranscriptStore::new(root.clone());
    fs::write(root.join(format!("ready-{index}")), b"ready").unwrap();
    let start = Instant::now();
    while !root.join("go").exists() {
        assert!(start.elapsed() < Duration::from_secs(10));
        thread::sleep(Duration::from_millis(5));
    }
    let id = if shared {
        "$shared".to_string()
    } else {
        format!("$process-{index}")
    };
    let mut record = agent_session_metadata(&id, &format!("process-{index}"));
    record.directive = Some("x".repeat(2000 + index * 2));
    for sequence in 0..20 {
        record.transcript_entries = sequence;
        store
            .save_agent_session_metadata_checkpoint(&id, std::slice::from_ref(&record))
            .unwrap();
    }
}

/// Empty checkpoints are committed deletion evidence. A later old-daemon global
/// rewrite must not resurrect a cleared binding during a fresh-store reopen.
#[test]
fn active_metadata_empty_namespace_never_resurrects_legacy_bindings() {
    let root = temp_root("active-metadata-empty");
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let old = agent_session_metadata("$old", "old-conversation");
    let original = format!("{}\n", encode_agent_session_metadata(&old).unwrap());
    fs::write(store.agent_session_metadata_file(), &original).unwrap();
    assert_eq!(
        store.load_agent_session_metadata("$old").unwrap(),
        vec![old]
    );
    store.save_agent_session_metadata("$old", &[]).unwrap();
    fs::write(store.agent_session_metadata_file(), original).unwrap();
    assert!(
        AgentTranscriptStore::new(root)
            .load_agent_session_metadata("$old")
            .unwrap()
            .is_empty()
    );
}

/// Scoped corruption is isolated from already-authoritative foreign sessions;
/// the selected damaged/future checkpoint cannot silently fall back to legacy.
/// Diagnostics must never echo private metadata field contents.
#[test]
fn active_metadata_corruption_is_scoped_and_diagnostics_are_content_free() {
    let root = temp_root("active-metadata-isolation");
    let store = AgentTranscriptStore::new(root.clone());
    let a = agent_session_metadata("$a", "conversation-a");
    let b = agent_session_metadata("$b", "conversation-b");
    store
        .save_agent_session_metadata("$a", std::slice::from_ref(&a))
        .unwrap();
    store
        .save_agent_session_metadata("$b", std::slice::from_ref(&b))
        .unwrap();
    let path = store.agent_session_metadata_checkpoint_file("$b");
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    envelope["version"] = 99.into();
    fs::write(&path, serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert_eq!(store.load_agent_session_metadata("$a").unwrap(), vec![a]);
    assert!(
        store
            .load_agent_session_metadata("$b")
            .unwrap_err()
            .message()
            .contains("version=99")
    );
    fs::write(
        store.agent_session_metadata_file(),
        b"PRIVATE_STEERING_MUST_NOT_LEAK\n",
    )
    .unwrap();
    let error = store
        .load_agent_session_metadata("$unimported")
        .unwrap_err();
    assert!(error.message().contains("line 1") && error.message().contains("fields=1"));
    assert!(!error.message().contains("PRIVATE_STEERING_MUST_NOT_LEAK"));
    assert!(
        !store
            .agent_session_metadata_checkpoint_file("$unimported")
            .exists()
    );
}

/// Failed private backup publication cannot be treated as successful recovery.
/// An exact retry reuses preserved bytes and installs one authoritative import.
#[test]
fn active_metadata_backup_failure_retains_source_and_allows_exact_retry() {
    let root = temp_root("active-metadata-backup-failure");
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root);
    let record = agent_session_metadata("$old", "old-conversation");
    let original = format!("{}\n]\n", encode_agent_session_metadata(&record).unwrap());
    fs::write(store.agent_session_metadata_file(), &original).unwrap();
    store.fail_active_metadata_phase_for_tests(5);
    assert!(store.load_agent_session_metadata("$old").is_err());
    assert!(
        !store
            .agent_session_metadata_checkpoint_file("$old")
            .exists()
    );
    assert_eq!(
        fs::read(store.agent_session_metadata_file()).unwrap(),
        original.as_bytes()
    );
    assert_eq!(
        store.load_agent_session_metadata("$old").unwrap(),
        vec![record]
    );
    assert!(
        store
            .agent_session_metadata_recovery_notice("$old")
            .unwrap()
            .unwrap()
            .contains("line 2")
    );
}

/// Publication failures before rename restore captured sidecars and keep the
/// old binding file; failures after rename retain consistent new captures and
/// explicitly report uncertain durability. No retry repeats provider/actions.
#[test]
fn active_metadata_publication_phases_preserve_checkpoint_sidecar_consistency() {
    for phase in 1..=4 {
        let root = temp_root("active-metadata-publication-failure");
        let store = AgentTranscriptStore::new(root.clone());
        let old = agent_session_metadata("$scope", "same-conversation");
        store
            .save_agent_session_metadata_checkpoint("$scope", std::slice::from_ref(&old))
            .unwrap();
        let mut new = old.clone();
        new.primary_display_name = Some("stablemachine".into());
        store.fail_active_metadata_phase_for_tests(phase);
        let error = store
            .save_agent_session_metadata_checkpoint("$scope", std::slice::from_ref(&new))
            .unwrap_err();
        let reopened = AgentTranscriptStore::new(root);
        if phase < 3 {
            assert_eq!(
                reopened.load_agent_session_metadata("$scope").unwrap(),
                vec![old]
            );
            assert_eq!(
                reopened
                    .conversation_primary_display_name("same-conversation")
                    .unwrap(),
                None
            );
        } else {
            assert!(
                error
                    .message()
                    .contains("published but durability is uncertain")
            );
            assert_eq!(
                reopened.load_agent_session_metadata("$scope").unwrap(),
                vec![new.clone()]
            );
            assert_eq!(
                reopened
                    .conversation_primary_display_name("same-conversation")
                    .unwrap()
                    .as_deref(),
                Some("stablemachine")
            );
        }
        assert_eq!(
            store
                .save_agent_session_metadata_checkpoint("$scope", &[new])
                .unwrap(),
            1
        );
    }
}

/// Shared legacy migration and current checkpoints reject symlink/special-node
/// aliases without reading their payload or blocking on a FIFO-like endpoint.
#[test]
fn active_metadata_rejects_namespace_and_checkpoint_symlink_aliases() {
    use std::os::unix::fs::symlink;
    let root = temp_root("active-metadata-symlink");
    fs::create_dir_all(root.join("outside")).unwrap();
    symlink(
        root.join("outside"),
        root.join(".active-agent-session-metadata-v1"),
    )
    .unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    assert!(store.load_agent_session_metadata("$x").is_err());
    fs::remove_file(root.join(".active-agent-session-metadata-v1")).unwrap();
    store.save_agent_session_metadata("$x", &[]).unwrap();
    let path = store.agent_session_metadata_checkpoint_file("$x");
    fs::remove_file(&path).unwrap();
    let outside = root.join("outside/source");
    fs::write(&outside, b"not checkpoint authority").unwrap();
    symlink(&outside, &path).unwrap();
    assert!(store.load_agent_session_metadata("$x").is_err());
    assert_eq!(fs::read(outside).unwrap(), b"not checkpoint authority");
    fs::remove_file(&path).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &path,
        rustix::fs::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    assert!(store.load_agent_session_metadata("$x").is_err());
}

/// A newer checkpoint must never touch an older daemon's shared index or its
/// still-open fixed temporary inode. Per-session publication must leave both
/// untouched even when the old process does not honor any new advisory lock.
#[test]
fn active_metadata_checkpoint_leaves_nonparticipating_legacy_writer_untouched() {
    let root = temp_root("active-metadata-old-writer");
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let original = format!(
        "{}\n",
        encode_agent_session_metadata(&agent_session_metadata("$old", "old-conversation")).unwrap()
    );
    let legacy = root.join("active-agent-sessions.tsv");
    let temp = legacy.with_extension("tmp");
    fs::write(&legacy, &original).unwrap();
    fs::write(&temp, b"older writer owns this inode\n").unwrap();
    let mut old_descriptor = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&temp)
        .unwrap();
    let new_record = agent_session_metadata("$new", "new-conversation");
    store
        .save_agent_session_metadata("$new", std::slice::from_ref(&new_record))
        .unwrap();
    assert_eq!(fs::read(&legacy).unwrap(), original.as_bytes());
    assert!(
        temp.is_file(),
        "new writer consumed the old writer's temp name"
    );
    old_descriptor.seek(SeekFrom::Start(0)).unwrap();
    let mut content = String::new();
    old_descriptor.read_to_string(&mut content).unwrap();
    assert_eq!(content, "older writer owns this inode\n");
    assert_eq!(
        store.load_agent_session_metadata("$new").unwrap(),
        vec![new_record]
    );
}

/// The exact incident shape (460 decodable rows then a standalone closing
/// bracket) must no longer abort a fresh daemon's binding read. Recovery keeps
/// every original byte in a private backup and never rewrites the live legacy
/// file, while a selected old session retains its complete validated metadata.
#[test]
fn active_metadata_legacy_trailing_bracket_is_preserved_and_imported() {
    let root = temp_root("active-metadata-bracket");
    fs::create_dir_all(&root).unwrap();
    let store = AgentTranscriptStore::new(root.clone());
    let selected = agent_session_metadata("$old", "selected-conversation");
    let mut original = format!("{}\n", encode_agent_session_metadata(&selected).unwrap());
    for index in 1..460 {
        original.push_str(&format!(
            "{}\n",
            encode_agent_session_metadata(&agent_session_metadata(
                &format!("$foreign-{index}"),
                &format!("conversation-{index}")
            ))
            .unwrap()
        ));
    }
    original.push_str("]\n");
    let legacy = root.join("active-agent-sessions.tsv");
    fs::write(&legacy, &original).unwrap();
    assert_eq!(
        store.load_agent_session_metadata("$old").unwrap(),
        vec![selected]
    );
    assert!(
        store
            .load_agent_session_metadata("$fresh")
            .unwrap()
            .is_empty()
    );
    assert_eq!(fs::read(&legacy).unwrap(), original.as_bytes());
    let backups = fs::read_dir(root.join(".active-agent-session-metadata-v1"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("legacy-")
                && path.extension().is_some_and(|extension| extension == "tsv")
        })
        .collect::<Vec<_>>();
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(&backups[0]).unwrap(), original.as_bytes());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
