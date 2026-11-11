//! Saved-fork worker publication and actor-only pane adoption.
//!
//! Immutable saved or NoStore source is selected before dispatch. The existing
//! fork generation owns target durability, adoption and conditional rollback;
//! missing replies never prove a possibly bound target can be deleted.

use super::*;

/// Bounded actor capture for saved-fork storage work. Historical contents never
/// carry authority; the new pane captures its own current action permissions.
#[derive(Debug, Clone)]
pub(crate) struct SavedForkWork {
    /// Store whose existing locks serialize source capture and target publication.
    pub(crate) store: AgentTranscriptStore,
    /// Attached primary that requested the new pane.
    primary_client_id: mez_core::ids::ClientId,
    /// Conversation whose exact active projection is inherited.
    source: String,
    /// Minimum physical audit coverage for a durable source. Frozen NoStore
    /// source carries its own exact canonical selection instead of this count.
    source_entries: u64,
    /// Actual NoStore selection and its original actor freshness fences.
    frozen: Option<std::sync::Arc<crate::runtime::control::RuntimeFrozenReplayCopySource>>,
    /// Fresh conversation id requested or generated at claim time.
    target: String,
    /// Original command used to exclude itself from the prompt seed.
    input: String,
    /// Fork-local archival creation timestamp.
    created_at: u64,
    /// Deadline/shutdown cancellation observed only by the fork storage worker.
    pub(crate) cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Independent service-stop signal; one job's deadline never poisons it.
    pub(crate) supervisor_cancelled: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

/// Worker-owned durable publication retained across both actor round trips.
/// The actor records adoption or proven safe compensation; it never deletes
/// storage. Missing replies and uncertain teardown deliberately retain history.
#[derive(Debug)]
pub(crate) struct SavedForkRead {
    /// Immutable source/target ownership captured before storage work.
    work: SavedForkWork,
    /// Exact created directory generation, held open until settlement finishes.
    publication: crate::storage::transcript::ForkPublication,
    /// Last submitted non-command prompt read on the worker.
    prompt_seed: Option<String>,
    /// Shared prompt recall loaded before actor adoption, with paste provenance.
    prompt_history: Vec<mez_mux::readline::ReadlineHistoryEntry>,
    /// Actor-only adoption decision, read by the same worker after its reply.
    adoption: std::sync::Mutex<SavedForkAdoption>,
    /// Owned-fixture post-bind and close uncertainty faults, never production policy.
    #[cfg(test)]
    test_fault: std::sync::atomic::AtomicU8,
    /// Exact test-owned child returned before any target binding, never authority.
    #[cfg(test)]
    test_returned_native_pid: std::sync::atomic::AtomicU32,
}

/// In-memory adoption decision; neither cancellation nor a missing reply is
/// evidence that deleting a possibly bound target is safe.
#[derive(Debug, Default)]
struct SavedForkAdoption {
    /// Permanent duplicate fence, retained even after the worker consumes its result.
    attempted: bool,
    /// True only when no adoption occurred or pane teardown completed successfully.
    cleanup_safe: bool,
    /// Final command body or setup failure, before worker compensation diagnostics.
    result: Option<Result<String>>,
}

/// Captures prompt history and publishes a validated archive/projection entirely
/// on the command worker. A failed publication compensates under storage locks.
pub(in crate::runtime::commands) fn read_saved_fork(work: &SavedForkWork) -> Result<SavedForkRead> {
    let prompt_history = work.store.structured_prompt_history(&work.source)?;
    let prompt_seed = RuntimeSessionService::runtime_agent_fork_prompt_seed(
        &work.store.prompt_history(&work.source)?,
        &work.input,
    );
    let mut cancellation = vec![work.cancelled.as_ref()];
    if let Some(supervisor) = &work.supervisor_cancelled {
        cancellation.push(supervisor.as_ref());
    }
    let publication = if let Some(frozen) = &work.frozen {
        let source = frozen.fork_source()?;
        if source.conversation_id() != work.source {
            return Err(MezError::conflict("saved fork frozen conversation changed"));
        }
        work.store.fork_frozen_owned_cancellable(
            &source,
            &work.target,
            work.created_at,
            &cancellation,
        )?
    } else {
        work.store.fork_owned_cancellable(
            &work.source,
            &work.target,
            work.created_at,
            &cancellation,
            work.source_entries,
        )?
    };
    Ok(SavedForkRead {
        work: work.clone(),
        publication,
        prompt_seed,
        prompt_history,
        adoption: std::sync::Mutex::new(SavedForkAdoption::default()),
        #[cfg(test)]
        test_fault: std::sync::atomic::AtomicU8::new(0),
        #[cfg(test)]
        test_returned_native_pid: std::sync::atomic::AtomicU32::new(0),
    })
}

impl RuntimeSessionService {
    /// Reports unavailable storage on the inline fallback. With storage enabled,
    /// admission must use the deferred owner and must never clone on the actor.
    pub(in crate::runtime::commands) fn execute_agent_shell_fork_command(
        &mut self,
        _primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        _input: &str,
    ) -> Result<AgentShellCommandOutcome> {
        if self.persistence.transcript_store().is_some() {
            return Err(MezError::invalid_state(
                "saved fork requires deferred storage execution",
            ));
        }
        let source = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?;
        Ok(AgentShellCommandOutcome::Display {
            command: "fork".into(),
            body: format!(
                "current_conversation={} forked=false reason=transcript-store-unavailable source=runtime-fork",
                json_escape(&source.session_id)
            ),
        })
    }

    /// Captures bounded ownership and command arguments without touching storage.
    pub(crate) fn prepare_saved_fork_work(
        &self,
        primary_client_id: &mez_core::ids::ClientId,
        pane_id: &str,
        input: &str,
    ) -> Result<SavedForkWork> {
        let invocation = parse_slash_command(input)?
            .ok_or_else(|| MezError::invalid_args("fork command must be a slash command"))?;
        let source = self
            .agent_shell_store()
            .get(pane_id)
            .ok_or_else(|| MezError::invalid_state("agent shell session not found for pane"))?
            .session_id
            .clone();
        // Unlike logical context replay, a saved fork promises a wholly durable
        // archive and projection. Queued and claimed writes remain owned by the
        // persistence component; never expose their shorter materialized prefix.
        if self.persistence.bookkeeping_pending(&source)
            || !self
                .persistence
                .pending_transcript_entries(&source)
                .is_empty()
        {
            return Err(MezError::conflict(
                "conversation persistence must settle before saved fork capture",
            ));
        }
        let target = invocation
            .args
            .split_whitespace()
            .next()
            .map(ToOwned::to_owned)
            .unwrap_or_else(Self::runtime_new_agent_conversation_id);
        Ok(SavedForkWork {
            store: self
                .persistence
                .cloned_transcript_store()
                .ok_or_else(|| MezError::invalid_state("saved-session store unavailable"))?,
            primary_client_id: primary_client_id.clone(),
            source_entries: self
                .agent_shell_store()
                .get(pane_id)
                .ok_or_else(|| MezError::invalid_state("saved fork source disappeared"))?
                .transcript_entries,
            frozen: crate::runtime::control::RuntimeFrozenReplayCopySource::capture(self, pane_id)?
                .map(std::sync::Arc::new),
            source,
            target,
            input: input.to_owned(),
            created_at: current_unix_seconds().max(1),
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            supervisor_cancelled: None,
        })
    }

    /// Applies the first actor round trip without settling the command claim.
    /// A stale or cancelled claim proves no pane adoption happened; all other
    /// failures are retained unless the setup transaction proves clean teardown.
    pub(crate) fn adopt_saved_fork_command(
        &mut self,
        work: &crate::runtime::RuntimeAgentCommandAsyncWork,
        read: &SavedForkRead,
    ) -> Result<bool> {
        let mut adoption = read.adoption.lock().map_err(|_| {
            MezError::invalid_state("saved fork adoption state poisoned; target retained")
        })?;
        if adoption.attempted {
            return Err(MezError::conflict(
                "saved fork adoption already attempted; target retained",
            ));
        }
        adoption.attempted = true;
        let current = self
            .agent_shell_store()
            .get(&work.pane_id)
            .is_some_and(|session| {
                session.session_id == work.conversation_id
                    && session.visibility == mez_agent::AgentShellVisibility::Visible
            });
        let store_current = self.persistence.transcript_store() == Some(&read.work.store);
        if !current
            || !store_current
            || read
                .work
                .frozen
                .as_ref()
                .is_some_and(|source| !source.is_current(self, &work.pane_id))
            || read
                .work
                .cancelled
                .load(std::sync::atomic::Ordering::Acquire)
            || read
                .work
                .supervisor_cancelled
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire))
            || !self
                .session
                .is_attached_primary(&read.work.primary_client_id)
            || !self.agent.agent_command_is_claimed(
                &work.pane_id,
                &work.conversation_id,
                work.claim_generation,
            )
        {
            adoption.cleanup_safe = true;
            adoption.result = Some(Err(MezError::conflict(
                "saved fork claim is stale or cancelled",
            )));
            return Ok(false);
        }
        // No pane operation has run yet. The installer revokes this proof just
        // before its first mutating split and re-establishes it only on rollback.
        adoption.cleanup_safe = true;
        let outcome = self.install_saved_fork_command(&work.pane_id, read, &mut adoption);
        adoption.result = Some(outcome.map(|outcome| {
            super::super::runtime_agent_shell_command_response_json(
                &work.pane_id,
                &work.input,
                Some(&outcome),
            )
        }));
        Ok(true)
    }

    /// Finishes the transaction on its original blocking worker. The receipt
    /// remains owned even when the actor reply is lost or a deadline expires.
    /// Uncertain adoption retains the target; proven non-adoption compensates
    /// only its exact generation and preserves primary plus cleanup failures.
    pub(crate) fn finish_saved_fork_worker(
        read: &SavedForkRead,
        actor_reply: Result<bool>,
    ) -> crate::runtime::RuntimeAgentCommandAsyncOutcome {
        let result = (|| -> Result<String> {
            if let Err(error) = actor_reply {
                // A queued request may arrive after this reply was lost. Fence
                // it before releasing the lease, even if no adoption ran yet.
                if let Ok(mut adoption) = read.adoption.lock() {
                    adoption.attempted = true;
                }
                return Err(MezError::new(
                    error.kind(),
                    format!(
                        "saved fork actor adoption reply unavailable; target retained: {error}",
                    ),
                ));
            }
            let (cleanup_safe, result) = {
                let mut adoption = read.adoption.lock().map_err(|_| {
                    MezError::invalid_state("saved fork adoption state poisoned; target retained")
                })?;
                (
                    adoption.cleanup_safe,
                    adoption.result.take().ok_or_else(|| {
                        MezError::invalid_state("saved fork adoption unproven; target retained")
                    })?,
                )
            };
            if cleanup_safe {
                match read
                    .work
                    .store
                    .compensate_fork_publication(&read.publication)
                {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(MezError::conflict(format!(
                            "saved fork setup failed: {}; target generation changed or absent; replacement retained",
                            result
                                .as_ref()
                                .err()
                                .map(ToString::to_string)
                                .unwrap_or_default(),
                        )));
                    }
                    Err(cleanup) => {
                        return Err(MezError::invalid_state(format!(
                            "saved fork setup failed: {}; target cleanup failed: {cleanup}",
                            result
                                .as_ref()
                                .err()
                                .map(ToString::to_string)
                                .unwrap_or_default(),
                        )));
                    }
                }
            }
            result
        })();
        let result = match read.publication.release_adoption_lease() {
            Ok(()) => result,
            Err(error) => Err(MezError::invalid_state(format!(
                "saved fork lease release failed: {error}; prior outcome={result:?}"
            ))),
        };
        match result {
            Ok(body) => crate::runtime::RuntimeAgentCommandAsyncOutcome::Response { body },
            Err(error) => crate::runtime::RuntimeAgentCommandAsyncOutcome::Failed {
                message: error.message().to_owned(),
                kind: error.kind(),
            },
        }
    }

    /// Selects a post-bind fixture failure: one cleans normally, two retains a
    /// live pane, and three reports uncertainty after the real pane close.
    /// Four returns the actual native guard before binding the target.
    /// Five fails after budget capture; six also retains the live target pane.
    #[cfg(test)]
    pub(crate) fn fail_saved_fork_adoption_for_tests(read: &SavedForkRead, fault: u8) {
        read.test_fault
            .store(fault, std::sync::atomic::Ordering::SeqCst);
    }

    /// Reports the exact child identity observed by the pre-bind fixture seam.
    #[cfg(test)]
    pub(crate) fn saved_fork_returned_native_pid_for_tests(read: &SavedForkRead) -> u32 {
        read.test_returned_native_pid
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Installs only live pane state from a worker-owned durable publication.
    /// Cleanup certainty is recorded before returning; storage compensation is
    /// performed later by the worker while it still owns the generation receipt.
    fn install_saved_fork_command(
        &mut self,
        pane_id: &str,
        read: &SavedForkRead,
        adoption: &mut SavedForkAdoption,
    ) -> Result<AgentShellCommandOutcome> {
        let primary_client_id = &read.work.primary_client_id;
        let source = &read.work.source;
        let summary = &read.publication.summary;
        let source_descriptor = self.find_pane_descriptor(pane_id).ok_or_else(|| {
            MezError::new(
                crate::error::MezErrorKind::NotFound,
                "source pane not found",
            )
        })?;
        let source_start_directory = self.agent_effective_working_directory(pane_id);
        let prompt_seed = read.prompt_seed.clone();
        let copied_shell_mode = self.agent_shell_mode_override(pane_id);
        let startup_mode = copied_shell_mode.unwrap_or_else(|| self.agent_default_shell_mode());
        let previous_primary_names = self.snapshot_primary_agent_names();
        let previous_budget = self.snapshot_runtime_budget_resume(&read.work.target);
        adoption.cleanup_safe = false;
        let previous_processes = self.tracked_runtime_pane_process_ids();
        let previous_panes = self
            .tracked_pane_descriptors()
            .into_iter()
            .map(|pane| pane.pane_id)
            .collect::<Vec<_>>();
        let started = match self.split_pane_in_window_with_process(
            primary_client_id,
            &source_descriptor.window_id,
            SplitDirection::Vertical,
            true,
            source_start_directory.as_deref(),
            crate::runtime::processes::RuntimePaneProcessPurpose::AgentOwned {
                shell_mode: startup_mode,
            },
        ) {
            Ok(started) => started,
            Err(error) => {
                adoption.cleanup_safe = self
                    .tracked_pane_descriptors()
                    .into_iter()
                    .map(|pane| pane.pane_id)
                    .collect::<Vec<_>>()
                    == previous_panes
                    && self.tracked_runtime_pane_process_ids() == previous_processes
                    && !self
                        .agent_shell_store()
                        .sessions()
                        .any(|session| session.session_id == read.work.target);
                return Err(error);
            }
        };
        let setup_result = (|| -> Result<_> {
            #[cfg(test)]
            if read.test_fault.load(std::sync::atomic::Ordering::SeqCst) == 4 {
                // Reuse the real handoff/Drop owners to model a startup guard
                // returned before this worker ever communicates the target id.
                let process = self.take_running_pane_process_for_adapter(&started.pane_id)?;
                read.test_returned_native_pid
                    .store(process.primary_pid(), std::sync::atomic::Ordering::SeqCst);
                drop(process);
                return Err(MezError::invalid_state(
                    "injected fork pre-binding native guard return",
                ));
            }
            #[cfg(test)]
            if self.take_agent_fork_after_persistence_failure_for_tests() {
                return Err(MezError::invalid_state(
                    "injected agent fork failure after persistence",
                ));
            }
            self.agent_shell_store_mut()
                .enter_or_resume(&started.pane_id)?;
            let (session_id, transcript_entries, visibility) = {
                let session = self
                    .agent_shell_store_mut()
                    .bind_conversation_with_lineage(
                        &started.pane_id,
                        &summary.conversation_id,
                        summary.entries as u64,
                        None,
                    )?;
                (
                    session.session_id.clone(),
                    session.transcript_entries,
                    session.visibility,
                )
            };
            #[cfg(test)]
            if (1..=3).contains(&read.test_fault.load(std::sync::atomic::Ordering::SeqCst)) {
                return Err(MezError::invalid_state(
                    "injected fork post-binding setup failure",
                ));
            }
            // A new generation has no saved display identity. Allocate from the
            // live reservation owner, never read inherited metadata on actor.
            let name = self.resolve_subagent_display_name(&format!("agent-{}", started.pane_id));
            self.install_primary_agent_name(&started.pane_id, Some(name))?;
            self.capture_agent_session_allowed_actions_for_pane(&started.pane_id)?;
            self.record_pane_transcript_ref(
                &started.pane_id,
                format!("transcript:{}:{session_id}", started.pane_id),
            )?;
            if let Some(shell_mode) = copied_shell_mode {
                self.set_agent_shell_mode_override(&started.pane_id, Some(shell_mode));
            }
            self.set_agent_prompt_history_for_pane(&started.pane_id, read.prompt_history.clone());
            self.sync_tracked_pty_sizes()?;
            let size = self
                .find_pane_descriptor(&started.pane_id)
                .ok_or_else(|| MezError::invalid_state("fork pane disappeared during setup"))?
                .size;
            self.ensure_agent_pane_screen(&started.pane_id, &session_id, size)?;
            // A fresh pane has no prior MMP objective to clear or durable mirror
            // to load. Register only its live identity; history grants no rights.
            self.ensure_runtime_message_identity(
                &format!("agent-{}", started.pane_id),
                None,
                "agent",
                &["agent-harness"],
                current_unix_seconds().saturating_mul(1000),
            )?;
            if startup_mode == crate::runtime::config::ShellMode::Native
                && !self.validate_native_agent_surface_startup(&started.pane_id)?
            {
                return Err(MezError::invalid_state(
                    "native fork startup owner is no longer current",
                ));
            }
            self.request_agent_prompt_selector_extra_candidates_refresh(&started.pane_id);
            if let Some(seed) = prompt_seed
                && let Some(prompt_input) = self.agent_prompt_input_mut(&started.pane_id)
            {
                prompt_input
                    .prompt
                    .buffer
                    .apply(ReadlineEdit::InsertText(seed));
            }
            // Fork metadata deliberately inherits no objective or authority.
            // The ordinary entry path has registered the fresh message identity.
            self.capture_runtime_budget_human_fork(
                primary_client_id,
                &started.pane_id,
                &session_id,
            )?;
            #[cfg(test)]
            if matches!(
                read.test_fault.load(std::sync::atomic::Ordering::SeqCst),
                5 | 6
            ) {
                return Err(MezError::invalid_state(
                    "injected fork failure after budget capture",
                ));
            }
            if self.persistence.transcript_uses_adapter() {
                // Persist adopted bindings independently of the source command's
                // eventual display claim. This queues owned work, never store I/O.
                self.checkpoint_agent_session_metadata()?;
            }
            Ok((session_id, transcript_entries, visibility))
        })();
        let (session_id, transcript_entries, visibility) = match setup_result {
            Ok(result) => result,
            Err(error) => {
                self.restore_primary_agent_names(previous_primary_names);
                #[cfg(test)]
                let skip_close = matches!(
                    read.test_fault.load(std::sync::atomic::Ordering::SeqCst),
                    2 | 6
                );
                #[cfg(not(test))]
                let skip_close = false;
                let pane_cleanup = if skip_close {
                    Err(MezError::invalid_state(
                        "injected close failure with live bound pane",
                    ))
                } else {
                    self.dispatch_runtime_pane_close(
                        primary_client_id,
                        &format!(
                            r#"{{"pane_id":"{}","force":true}}"#,
                            json_escape(&started.pane_id)
                        ),
                    )
                };
                #[cfg(test)]
                let pane_cleanup = if read.test_fault.load(std::sync::atomic::Ordering::SeqCst) == 3
                {
                    Err(MezError::invalid_state(
                        "injected close failure after pane removal",
                    ))
                } else {
                    pane_cleanup
                };
                adoption.cleanup_safe = pane_cleanup.is_ok()
                    && self.find_pane_descriptor(&started.pane_id).is_none()
                    && self.agent_shell_store().get(&started.pane_id).is_none()
                    && !self
                        .tracked_runtime_pane_process_ids()
                        .contains(&started.pane_id)
                    && !self.pane_process_is_adapter_owned(&started.pane_id);
                if !adoption.cleanup_safe {
                    return Err(MezError::invalid_state(format!(
                        "agent fork setup failed: {error}; rollback failed: pane={}; storage=retained because teardown is uncertain",
                        pane_cleanup
                            .err()
                            .map(|cleanup| cleanup.to_string())
                            .unwrap_or_else(|| "pane, process or binding remains".to_string()),
                    )));
                }
                self.rollback_runtime_budget_resume(previous_budget);
                return Err(error);
            }
        };
        Ok(AgentShellCommandOutcome::Mutated {
            command: "fork".to_string(),
            body: format!(
                "source={} conversation_id={} entries={} source_pane={} pane={} forked=true",
                source, session_id, transcript_entries, pane_id, started.pane_id
            ),
            visibility,
        })
    }

    /// Returns the prompt text that should seed a newly forked agent pane.
    ///
    /// # Parameters
    /// - `history`: Shared persisted agent prompt history for the source
    ///   conversation.
    /// - `current_input`: The `/fork` command currently being executed.
    fn runtime_agent_fork_prompt_seed(history: &[String], current_input: &str) -> Option<String> {
        let current = current_input.trim();
        history
            .iter()
            .rev()
            .find(|entry| {
                let trimmed = entry.trim();
                !trimmed.is_empty() && (current.is_empty() || trimmed != current)
            })
            .cloned()
    }
}
