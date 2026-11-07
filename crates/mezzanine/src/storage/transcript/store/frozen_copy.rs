//! Saved copies of immutable NoStore source under the existing fork generation.
//!
//! The captured model and original row proposals have no committed parent
//! identity. This worker commits the child's independent initial projection
//! while holding its target lease and rollback journal. It creates no parent
//! files, append receipt, effect queue or controller capability.

use super::*;

impl AgentTranscriptStore {
    /// Publishes the exact frozen source as a fresh saved conversation. Until
    /// completion, the existing active-delete intent owns all partial child
    /// files. Returned generation ownership fences actor adoption/compensation.
    pub(crate) fn fork_frozen_owned_cancellable(
        &self,
        source: &super::super::replay_epoch::ReplayForkSource,
        target: &str,
        created_at: u64,
        cancelled: &[&AtomicBool],
    ) -> Result<ForkPublication> {
        let check_cancelled = || {
            if cancelled.iter().any(|flag| flag.load(Ordering::Acquire)) {
                Err(MezError::invalid_state(
                    "saved frozen fork cancelled before adoption",
                ))
            } else {
                Ok(())
            }
        };
        check_cancelled()?;
        let batch = source.initial_batch(target, created_at)?;
        let lease = self.acquire_fork_conversation_lock(target, cancelled)?;
        self.check_fork_target_available_locked(target)?;
        if self.has_unsettled_append_receipts_locked(target, &lease)? {
            return Err(MezError::conflict(
                "saved fork target append ownership is unsettled",
            ));
        }
        check_cancelled()?;
        self.begin_fork_publication(target)?;
        let result: Result<ForkPublication> = (|| {
            // The original fork generation is this initial bundle's actual
            // owner. Reuse the held lane; public append APIs would relock it.
            self.reconcile_initial_projection_locked(&batch, &lease)?;
            self.verify_initial_projection_committed_locked(&batch)?;
            check_cancelled()?;
            self.complete_fork_publication(target)?;
            self.upsert_catalog_from_files(target, None)?;
            let summary = self
                .saved_session(target)?
                .map(|session| session.summary)
                .ok_or_else(|| MezError::invalid_state("frozen fork summary missing"))?;
            let directory = Arc::new(std_fs::File::open(self.session_dir_path(target)?)?);
            Ok(ForkPublication {
                summary,
                directory,
                lease: Arc::new(Mutex::new(None)),
            })
        })();
        let publication = match result {
            Ok(publication) => publication,
            Err(error) => {
                return match self.compensate_failed_fork_locked(target) {
                    Ok(_) => Err(error),
                    Err(cleanup) => Err(MezError::new(
                        error.kind(),
                        format!("frozen fork failed: {error}; target cleanup failed: {cleanup}",),
                    )),
                };
            }
        };
        *publication
            .lease
            .lock()
            .map_err(|_| MezError::invalid_state("fork lease poisoned"))? = Some(lease);
        Ok(publication)
    }

    /// Both saved-copy publishers must prove target absence under its existing
    /// mutation lease, including names, catalog claims and recovery artifacts.
    pub(super) fn check_fork_target_available_locked(&self, target: &str) -> Result<()> {
        if self.conversation_exists(target)?
            || self.fork_target_recovery_artifacts_exist(target)?
            || (catalog::catalog_path(self).exists()
                && catalog::record_for_mutation(self, target)?.is_some())
            || self.read_named_sessions_index()?.contains_key(target)
        {
            return Err(MezError::conflict("target conversation already exists"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::transcript::replay_epoch::{
        Phase, ReplayForkSource, ReplayMemorySource, ReplaySourceOrigin,
    };
    use mez_agent::{
        AgentContext, ContextBlock, ContextBlockOccurrenceId, ReplayCompactionPreparation,
        ReplayConversationId, ReplayEpochContext, ReplayHeadPublication, ReplayTailPolicy,
    };

    struct Fixture {
        root: PathBuf,
        store: AgentTranscriptStore,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "mez-frozen-saved-fork-{}",
                String::from(ContextBlockOccurrenceId::allocate()),
            ));
            Self {
                store: AgentTranscriptStore::new(&root),
                root,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std_fs::remove_dir_all(&self.root);
        }
    }

    /// Real canonical task admission keeps its current instruction protected;
    /// only the preceding neutral source can enter the supplied summary.
    fn source(headed: bool) -> ReplayForkSource {
        let mut owner = ReplayEpochContext::new(
            ReplayConversationId::new("memory-parent").unwrap(),
            AgentContext::empty(),
        )
        .unwrap();
        owner
            .append_fact("original fact", "completed original source")
            .unwrap();
        owner
            .admit_instruction_task(
                vec![ContextBlock::user_event(
                    "user prompt",
                    "current memory task",
                )],
                &[],
                &[],
                0,
            )
            .unwrap();
        let owner = if headed {
            let plan = match owner
                .prepare_compaction(ReplayTailPolicy::new(1000, 0).unwrap())
                .unwrap()
            {
                ReplayCompactionPreparation::Ready(plan) => plan,
                other => panic!("expected closed fact before current task: {other:?}"),
            };
            owner
                .prepare_compaction_publication(
                    plan,
                    ReplayHeadPublication::Summary("actual memory head".into()),
                )
                .unwrap()
                .candidate()
                .clone()
        } else {
            owner
        };
        let origin = ReplaySourceOrigin::new("memory-turn", "memory-agent", "memory-pane").unwrap();
        ReplayMemorySource::capture(&owner, None, None, Some((&origin, 0)), &[])
            .unwrap()
            .fork_source(&owner)
            .unwrap()
    }

    #[test]
    fn frozen_saved_fork_keeps_raw_and_headed_sources_independent() {
        for headed in [false, true] {
            let fixture = Fixture::new();
            let source = source(headed);
            let publication = fixture
                .store
                .fork_frozen_owned_cancellable(&source, "saved-child", 123, &[])
                .unwrap();
            publication.release_adoption_lease().unwrap();
            assert_eq!(
                fixture.store.inspect("saved-child").unwrap().len(),
                source.rows().len()
            );
            assert_eq!(
                fixture
                    .store
                    .load_replay_epoch("saved-child", AgentContext::empty())
                    .unwrap()
                    .is_some(),
                headed
            );
            assert!(!fixture.root.join("memory-parent").exists());
            assert!(
                !fixture
                    .store
                    .has_unsettled_append_receipts("saved-child")
                    .unwrap()
            );
        }
    }

    /// Every uncertain epoch phase belongs to this new target's rollback
    /// generation; it must not strand an append receipt or save the parent.
    #[test]
    fn frozen_saved_fork_faults_compensate_target_and_allow_retry() {
        for phase in [
            Phase::BeforePayloadSync,
            Phase::BeforeSourceReceiptSync,
            Phase::BeforeManifestSwap,
            Phase::AfterManifestSwap,
            Phase::BeforeRequiredMarker,
            Phase::BeforeAcknowledgment,
        ] {
            let fixture = Fixture::new();
            let source = source(true);
            fixture.store.inject_replay_epoch_fault(phase, 1, false);
            assert!(
                fixture
                    .store
                    .fork_frozen_owned_cancellable(&source, "retry-child", 123, &[])
                    .is_err(),
                "{phase:?}"
            );
            assert!(!fixture.root.join("retry-child").exists(), "{phase:?}");
            assert!(
                !fixture
                    .store
                    .fork_target_recovery_artifacts_exist("retry-child")
                    .unwrap()
            );
            assert!(
                !fixture
                    .store
                    .has_unsettled_append_receipts("retry-child")
                    .unwrap()
            );
            let retry = fixture
                .store
                .fork_frozen_owned_cancellable(&source, "retry-child", 124, &[])
                .unwrap();
            retry.release_adoption_lease().unwrap();
            assert!(
                fixture
                    .store
                    .load_replay_epoch("retry-child", AgentContext::empty())
                    .unwrap()
                    .is_some()
            );
            assert!(!fixture.root.join("memory-parent").exists());
        }
    }

    /// Exhausted immediate cleanup keeps both diagnostics and the original
    /// rollback intent. Another target remains usable; restart removes only
    /// the incomplete child, without ever persisting its NoStore parent.
    #[test]
    fn frozen_saved_fork_cleanup_failure_recovers_only_its_target() {
        let fixture = Fixture::new();
        let source = source(true);
        fixture
            .store
            .inject_replay_epoch_fault(Phase::BeforeManifestSwap, 1, false);
        fixture.store.fail_next_fork_cleanup();
        let error = fixture
            .store
            .fork_frozen_owned_cancellable(&source, "incomplete-child", 123, &[])
            .unwrap_err();
        let diagnostic = error.to_string();
        assert!(diagnostic.contains("frozen fork failed"), "{diagnostic}");
        assert!(diagnostic.contains("target cleanup failed"), "{diagnostic}");
        assert!(
            diagnostic.contains("injected fork target cleanup failure"),
            "{diagnostic}"
        );
        assert!(fixture.root.join("incomplete-child").exists());
        assert!(
            fixture
                .store
                .saved_session("incomplete-child")
                .unwrap()
                .is_none()
        );
        let other = fixture
            .store
            .fork_frozen_owned_cancellable(&source, "independent-child", 124, &[])
            .unwrap();
        other.release_adoption_lease().unwrap();
        let path = fixture.root.join("independent-child/compaction-epoch.json");
        let before = std_fs::read(&path).unwrap();
        let reopened = AgentTranscriptStore::new(&fixture.root);
        reopened.recover_archive_transactions().unwrap();
        assert!(!fixture.root.join("incomplete-child").exists());
        assert_eq!(std_fs::read(path).unwrap(), before);
        assert!(
            reopened
                .load_replay_epoch("independent-child", AgentContext::empty())
                .unwrap()
                .is_some()
        );
        assert!(!fixture.root.join("memory-parent").exists());
    }

    #[test]
    fn frozen_saved_fork_collision_and_cancellation_preserve_owned_state() {
        let fixture = Fixture::new();
        let source = source(true);
        let cancelled = AtomicBool::new(true);
        assert!(
            fixture
                .store
                .fork_frozen_owned_cancellable(&source, "cancelled-child", 123, &[&cancelled])
                .is_err()
        );
        assert!(!fixture.root.join("cancelled-child").exists());
        assert!(
            fixture
                .store
                .fork_frozen_owned_cancellable(&source, "memory-parent", 123, &[])
                .is_err()
        );
        let first = fixture
            .store
            .fork_frozen_owned_cancellable(&source, "existing-child", 123, &[])
            .unwrap();
        first.release_adoption_lease().unwrap();
        let path = fixture.root.join("existing-child/compaction-epoch.json");
        let before = std_fs::read(&path).unwrap();
        assert!(
            fixture
                .store
                .fork_frozen_owned_cancellable(&source, "existing-child", 456, &[])
                .is_err()
        );
        assert_eq!(std_fs::read(path).unwrap(), before);
        assert!(!fixture.root.join("memory-parent").exists());
    }
}
