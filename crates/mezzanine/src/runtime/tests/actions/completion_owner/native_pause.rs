//! N-class pause/archive coverage with preserving pre-start native custody.
//!
//! Only this module starts a real fixture root. The existing synchronous
//! completion admission and archive oracles remain unchanged; no actor adapters,
//! provider, HTTP/MCP worker, approval bypass or fabricated lifecycle state is
//! introduced. Storage and an external native owner exist before startup and
//! survive constructor unwind. Cleanup uses the original captured attempt and
//! absolute deadline, never a late destructor signal or a numeric PID reopen.
//! Unknown cleanup retains the same native owner and all generated storage.

use super::*;
use futures_util::FutureExt;
use mez_mux::process::{PaneProcessCleanupAttempt, PaneProcessRetirementOwner};
use std::any::Any;
use std::mem::ManuallyDrop;
use std::os::unix::fs::DirBuilderExt;
use std::panic::AssertUnwindSafe;
use tokio::time::Instant;

/// Owns original typed setup failures and opaque panic payloads without cloning.
enum NativePauseFailure {
    /// Original runtime/startup/native-drain failure.
    Error(MezError),
    /// Original panic at the named setup or cleanup boundary.
    Panic(&'static str, Box<dyn Any + Send>),
    /// The unchanged original absolute fixture budget expired.
    Deadline,
    /// An explicit missing proof, never a manufactured success receipt.
    Boundary(&'static str),
    /// Generated storage could not be removed after successful settlement.
    Filesystem(std::io::Error),
}

impl std::fmt::Debug for NativePauseFailure {
    /// Does not invoke an arbitrary panic payload's display implementation.
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error(error) => output.debug_tuple("Error").field(error).finish(),
            Self::Panic(stage, payload) => output
                .debug_tuple("Panic")
                .field(stage)
                .field(&payload.as_ref().type_id())
                .finish(),
            Self::Deadline => output.write_str("Deadline"),
            Self::Boundary(reason) => output.debug_tuple("Boundary").field(reason).finish(),
            Self::Filesystem(error) => output.debug_tuple("Filesystem").field(error).finish(),
        }
    }
}

/// Preserves setup/oracle primary ownership independently from cleanup failures.
struct NativePauseReport {
    /// Original error/panic/deadline; cleanup cannot overwrite it.
    primary: Option<NativePauseFailure>,
    /// Independent selector, release, drain and storage failures.
    cleanup: Vec<NativePauseFailure>,
    /// This exclusively created generation remains available on any failure.
    root: PathBuf,
}

impl std::fmt::Debug for NativePauseReport {
    /// Reads owned evidence without invoking an arbitrary original panic codec.
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        output
            .debug_struct("NativePauseReport")
            .field("primary", &self.primary)
            .field("cleanup", &self.cleanup)
            .field("root", &self.root)
            .finish()
    }
}

/// Purpose-local preservation using the same core contracts as NativeFixture.
/// ManuallyDrop deliberately retains the external owner on unknown/cancellation;
/// PathBuf destruction never deletes storage. There is no destructor I/O.
struct NativePauseGuard {
    /// Exclusively generated storage, never an ambient directory.
    root: PathBuf,
    /// Private child environment; no user startup/configuration is inherited.
    environment: Vec<(String, String)>,
    /// Captured before spawn, shared by setup, observations and cleanup.
    deadline: Instant,
    /// Exact service owner retained externally before native acquisition.
    native: Option<ManuallyDrop<PaneProcessRetirementOwner>>,
    /// First-source cleanup token; never recaptured during recovery.
    attempt: Option<PaneProcessCleanupAttempt>,
    /// PID returned by actual startup, not authority inferred from a number.
    started_pid: Option<u32>,
    /// Real stable-exec identity captured before turn admission and claims.
    identity: Option<crate::runtime::processes::RuntimePaneProcessIdentity>,
    /// Observation that this original root occupies the sole attached slot.
    attached_root: bool,
    /// Prevents a second explicit cleanup attempt by this fixture.
    retired: bool,
}

impl NativePauseGuard {
    /// Creates a private generation before any native startup can acquire work.
    fn new(deadline: Instant) -> Self {
        // temp_root already creates/removes its named directory. Native data
        // needs create-only ownership: never remove a former failed generation
        // if a PID/name is reused, and never adopt an existing directory.
        let unique = NEXT_TEMP_ROOT_ID.fetch_add(1, Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("native fixture naming clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "mez-native-pause-{}-{unique}-{nonce}",
            std::process::id()
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let mut environment = Vec::new();
        for (key, name) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_RUNTIME_DIR", "runtime"),
        ] {
            let path = root.join(name);
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            environment.push((key.to_string(), path.to_string_lossy().into_owned()));
        }
        environment.extend([
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
            ("TERM".into(), "xterm-256color".into()),
            ("LANG".into(), "C.UTF-8".into()),
            ("TMPDIR".into(), "/tmp".into()),
        ]);
        Self {
            root,
            environment,
            deadline,
            native: None,
            attempt: None,
            started_pid: None,
            identity: None,
            attached_root: false,
            retired: false,
        }
    }

    /// Borrows the original externally preserved owner, when installed.
    fn owner(&self) -> Option<&PaneProcessRetirementOwner> {
        self.native.as_deref()
    }

    /// Installs the actual service owner and first-source token BEFORE spawn.
    /// Failure still leaves the same external owner/storage preserved.
    fn install(&mut self, service: &RuntimeSessionService) -> Result<()> {
        if self.native.is_some() {
            return Err(MezError::invalid_state(
                "native pause owner already installed",
            ));
        }
        self.native = Some(ManuallyDrop::new(service.native_pane_retirement_owner()));
        let attempt = self
            .owner()
            .expect("owner installed above")
            .capture_cleanup_attempt(self.deadline.into_std())
            .map_err(MezError::from)?;
        self.attempt = Some(attempt);
        Ok(())
    }

    /// Starts one real manager-owned exec root before any turn/claim exists.
    /// Waits only for a genuine stable kernel exec witness, within the original
    /// deadline; it never refreshes an already captured management target.
    async fn start(&mut self, service: &mut RuntimeSessionService) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(MezError::invalid_state(
                "native pause startup original deadline exhausted",
            ));
        }
        let started = service.start_initial_pane_process_with_launch_context(
            Some("exec /bin/cat >/dev/null"),
            Some(&self.root),
            Some(&self.environment),
        )?;
        self.started_pid = Some(started.primary_pid);
        let expected = fs::canonicalize("/bin/cat").map_err(|error| {
            MezError::invalid_state(format!("native pause cat witness: {error}"))
        })?;
        loop {
            if let Ok(identity) = service.pane_process_identity("%1")
                && identity.process_id == started.primary_pid
                && identity.executable_path == expected
            {
                if service.manager_owned_pane_process_pid("%1") != Some(started.primary_pid)
                    || service.pane_process_is_adapter_owned("%1")
                    || self.owner().is_none_or(|owner| owner.outstanding() != 1)
                {
                    return Err(MezError::invalid_state(
                        "native pause root lacks sole manager/attached-owner proof",
                    ));
                }
                self.identity = Some(identity);
                self.attached_root = true;
                return Ok(());
            }
            if Instant::now() >= self.deadline {
                return Err(MezError::invalid_state(
                    "native pause stable exec witness unavailable within original deadline",
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }

    /// Independently joins selector writers, returns service guards and drains
    /// the SAME owner/token/deadline. Successful selector return proves thread
    /// quiescence, not worker success or an interruptible filesystem deadline.
    /// Every failed/unknown case preserves storage and the external owner.
    async fn finish(
        &mut self,
        fixture: &mut Option<Box<CompletionFixture>>,
        case_succeeded: bool,
    ) -> Vec<NativePauseFailure> {
        if self.retired {
            return vec![NativePauseFailure::Boundary(
                "native pause cleanup already entered",
            )];
        }
        self.retired = true;
        let mut cleanup = Vec::new();
        let mut selectors_quiet = false;
        let mut manager_empty = false;
        if let Some(fixture) = fixture.as_mut() {
            match std::panic::catch_unwind(AssertUnwindSafe(|| {
                fixture.service.clear_agent_prompt_inputs()
            })) {
                Ok(()) => selectors_quiet = true,
                Err(payload) => cleanup.push(NativePauseFailure::Panic("selector join", payload)),
            }
            if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(|| {
                fixture.service.release_native_panes_for_retirement()
            })) {
                cleanup.push(NativePauseFailure::Panic("native handle release", payload));
            }
            manager_empty = fixture
                .service
                .tracked_runtime_pane_process_ids()
                .is_empty();
            if !manager_empty {
                cleanup.push(NativePauseFailure::Boundary(
                    "manager retained native handles after release",
                ));
            }
        } else {
            // Constructor unwind returns native guards to the externally held
            // slot owner. Without the service, writer quiescence is unproven;
            // preserve data even if the independent native drain succeeds.
            cleanup.push(NativePauseFailure::Boundary(
                "constructor did not return service/selector proof",
            ));
        }
        drop(fixture.take());
        let mut native_settled = false;
        if let Some(owner) = self.owner() {
            owner.seal();
            if Instant::now() >= self.deadline {
                cleanup.push(NativePauseFailure::Deadline);
            } else if let Some(attempt) = self.attempt.as_ref() {
                match AssertUnwindSafe(
                    crate::host::async_runtime::drain_native_pane_retirement_for_attempt(
                        owner,
                        attempt,
                        self.deadline,
                    ),
                )
                .catch_unwind()
                .await
                {
                    Ok(Ok(())) => {
                        native_settled = owner.outstanding() == 0 && !owner.has_cleanup_failure()
                    }
                    Ok(Err(error)) => cleanup.push(NativePauseFailure::Error(error)),
                    Err(payload) => cleanup.push(NativePauseFailure::Panic(
                        "native original-attempt drain",
                        payload,
                    )),
                }
            } else {
                cleanup.push(NativePauseFailure::Boundary(
                    "original native cleanup token unavailable",
                ));
            }
            if owner.outstanding() != 0 || owner.has_cleanup_failure() {
                cleanup.push(NativePauseFailure::Boundary(
                    "native ownership remains outstanding or sticky",
                ));
            }
        } else {
            cleanup.push(NativePauseFailure::Boundary(
                "pre-start external native owner unavailable",
            ));
        }
        if !self.attached_root
            || self
                .identity
                .as_ref()
                .is_none_or(|identity| Some(identity.process_id) != self.started_pid)
        {
            cleanup.push(NativePauseFailure::Boundary(
                "actual started identity/attached-root evidence unavailable",
            ));
        }
        if !native_settled || !selectors_quiet || !manager_empty {
            cleanup.push(NativePauseFailure::Boundary(
                "independent native/writer/manager settlement unproven",
            ));
        }
        // The last successful wait/descheduling can cross the budget even
        // when the drain observes zero outstanding children. Recheck the SAME
        // absolute deadline before deletion/owner-release eligibility.
        if !cleanup_observed_before_deadline(self.deadline, Instant::now())
            && !cleanup
                .iter()
                .any(|failure| matches!(failure, NativePauseFailure::Deadline))
        {
            cleanup.push(NativePauseFailure::Deadline);
        }
        if case_succeeded && cleanup.is_empty() {
            // The externally held original owner cannot reach final Drop while
            // proof is inspected. Its attached slot clears only through a real
            // wait; an already-empty drain is not described as a new PID wait.
            match fs::remove_dir_all(&self.root) {
                Ok(()) => {
                    if let Some(owner) = self.native.take() {
                        drop(ManuallyDrop::into_inner(owner));
                    }
                }
                Err(error) => cleanup.push(NativePauseFailure::Filesystem(error)),
            }
        }
        cleanup
    }
}

/// Pure final-budget gate shared with the process-free boundary regression.
/// Neither argument is a native receipt or replacement clock/token: production
/// passes the original deadline and the observation AFTER cleanup awaited.
pub(super) fn cleanup_observed_before_deadline(deadline: Instant, observed_at: Instant) -> bool {
    observed_at < deadline
}

/// Preserves the original pause/archive/continuation oracle in synchronous
/// service mode; the caller supplies actual pre-admission native identity.
fn assert_paused_outcome(
    fixture: &mut CompletionFixture,
    identity: &crate::runtime::processes::RuntimePaneProcessIdentity,
    failed: bool,
    held: bool,
) {
    let original = fixture
        .service
        .persistence
        .cloned_transcript_store()
        .unwrap();
    if failed && held {
        // Verbose shell-view mode hides read bodies; Debug reaches this actual
        // visible provisional install/retirement cut without changing policy.
        fixture
            .service
            .agent_shell_store_mut()
            .set_log_level("%1", AgentLogLevel::Debug)
            .unwrap();
        let progress = mez_agent::ActionPresentationProgress::new(
            &fixture.turn.turn_id,
            &fixture.action.id,
            mez_agent::ActionPresentationExecutionIdentity::Attempt(fixture.attempt.clone()),
            1,
            mez_agent::ActionPresentationComponentIdentity::ProvisionalReadBody,
            "provisional failed fetch bytes",
        );
        assert!(
            fixture
                .service
                .apply_action_presentation_progress(progress)
                .unwrap()
        );
    }
    let target = fixture
        .service
        .capture_agent_lifecycle_target(&fixture.primary, "%1")
        .unwrap();
    assert_eq!(target.process.as_ref(), Some(identity));
    let generation = fixture
        .service
        .pause_agent_lifecycle_target(&fixture.primary, &target)
        .unwrap();
    fixture
        .service
        .hold_agent_presentation_installs_for_tests(held);
    let mut outcome = fixture.outcome();
    if failed {
        outcome.result = Ok(ActionResult::failed(
            &fixture.turn,
            &fixture.action,
            ActionStatus::Failed,
            "fixture_paused_failure",
            "actual failed result remains failed",
        )
        .unwrap());
    }
    assert!(
        fixture
            .service
            .complete_approved_external_action(outcome)
            .unwrap()
    );
    fixture.service.reconcile_human_pauses().unwrap();
    assert_eq!(
        fixture.service.agent_human_pause_status("%1"),
        Some("paused")
    );
    assert!(
        !fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    let replacement = AgentTranscriptStore::new(fixture._root.0.join("resume-replacement"));
    fixture
        .service
        .set_agent_transcript_store(replacement.clone());
    let target = fixture
        .service
        .capture_agent_lifecycle_target(&fixture.primary, "%1")
        .unwrap();
    assert_eq!(target.process.as_ref(), Some(identity));
    assert!(
        fixture
            .service
            .resume_agent_lifecycle_target(&fixture.primary, &target, generation)
            .unwrap()
    );
    if held {
        assert!(
            !fixture
                .service
                .agent_provider_task_is_pending(&fixture.turn.turn_id)
        );
        assert!(
            fixture
                .service
                .completion_publication_retry_needed_for_turn(&fixture.turn.turn_id)
        );
        assert_eq!(fixture.service.retry_completion_publications(), 0);
        assert_eq!(fixture.service.retry_completion_publications(), 0);
        fixture
            .service
            .hold_agent_presentation_installs_for_tests(false);
        assert_eq!(fixture.service.retry_completion_publications(), 1);
        assert_eq!(fixture.service.retry_completion_publications(), 0);
    }
    assert!(
        fixture
            .service
            .agent_provider_task_is_pending(&fixture.turn.turn_id)
    );
    let execution = fixture.retained();
    assert_eq!(
        execution.action_results[0].status,
        if failed {
            ActionStatus::Failed
        } else {
            ActionStatus::Succeeded
        }
    );
    assert_eq!(execution.action_results[1].status, ActionStatus::Cancelled);
    let rows = original.inspect(&fixture.turn.conversation_id).unwrap();
    assert_eq!(
        rows.iter()
            .filter(|row| row.content.contains("action result pending-sibling"))
            .count(),
        1
    );
    assert!(rows.iter().any(|row| {
        row.content
            .contains("Continue your retained task from its current state")
    }));
    if failed {
        assert!(
            rows.iter()
                .any(|row| row.content.contains("actual failed result remains failed"))
        );
    }
    assert!(
        !replacement
            .transcript_path(&fixture.turn.conversation_id)
            .unwrap()
            .exists()
    );
    let agent = AgentId::opaque(fixture.turn.agent_id.clone()).unwrap();
    assert!(
        fixture
            .service
            .ready_agent_provider_turn(&agent, &fixture.turn.turn_id)
            .unwrap()
            .is_some()
    );
    assert!(
        !fixture
            .service
            .admit_completion_provider_continuation(&fixture.turn.turn_id)
            .unwrap(),
        "readiness and repeat admission cannot insert another continuation"
    );
    fixture.service.ensure_completion_admission_open().unwrap();
}

/// Exact human resume retains settled status, cancels unissued siblings and
/// archives original-store guidance before one continuation. This is the OLD
/// root-module function moved to native_pause with the SAME name/oracles; the
/// genuine root is now acquired before turn/claim, never faked. All four
/// success/failure x held/unheld variants require N allocation. No external,
/// provider or native command worker is executed; the real cat root is N work.
#[tokio::test]
async fn runtime_completion_owner_paused_outcome_resumes_with_original_archive_evidence() {
    for (failed, held) in [(false, false), (true, false), (false, true), (true, true)] {
        let deadline = Instant::now() + std::time::Duration::from_secs(30);
        let mut guard = NativePauseGuard::new(deadline);
        let mut fixture = None;
        // Box this concrete setup future/returned fixture, not the harness or
        // its stack settings. Transient construction is still not stack-free.
        let result = tokio::time::timeout_at(
            deadline,
            AssertUnwindSafe(Box::pin(async {
                fs::create_dir(guard.root.join("transcripts")).unwrap();
                let mut service = RuntimeServiceFixture::new()
                    .control_socket(guard.root.join("control.sock"))
                    .build();
                service.set_agent_transcript_store(AgentTranscriptStore::new(
                    guard.root.join("transcripts"),
                ));
                let primary = service
                    .attach_primary("primary", true, Size::new(120, 32).unwrap(), 120)
                    .unwrap();
                guard.install(&service)?;
                guard.start(&mut service).await?;
                let root = CompletionRoot(
                    guard.root.clone(),
                    CompletionRootCleanup::PreserveForNativeOwner,
                );
                fixture = Some(Box::new(
                    Box::pin(CompletionFixture::admit_prepared(
                        service, primary, root, true,
                    ))
                    .await,
                ));
                assert_paused_outcome(
                    fixture.as_mut().unwrap(),
                    guard.identity.as_ref().unwrap(),
                    failed,
                    held,
                );
                Ok::<(), MezError>(())
            }))
            .catch_unwind(),
        )
        .await;
        let primary = match result {
            Ok(Ok(Ok(()))) => None,
            Ok(Ok(Err(error))) => Some(NativePauseFailure::Error(error)),
            Ok(Err(payload)) => Some(NativePauseFailure::Panic(
                "native pause setup/oracle",
                payload,
            )),
            Err(_) => Some(NativePauseFailure::Deadline),
        };
        let cleanup = guard.finish(&mut fixture, primary.is_none()).await;
        if primary.is_some() || !cleanup.is_empty() {
            let report = NativePauseReport {
                primary,
                cleanup,
                root: guard.root.clone(),
            };
            eprintln!("native pause failed={failed} held={held}: {report:?}");
            std::panic::resume_unwind(Box::new(report));
        }
    }
}
