//! OS-backed pane process identity and typed pane shell identity evidence.
//!
//! Pane shell identity must come from the pane's own adapter-owned process or
//! from a receiver installed by the runtime's authenticated managed handshake.
//! Probe frames, bootstrap environment records, and session spawn records are
//! correlation evidence: they can select how a transaction is rendered or
//! which managed child is staged, but they can never attest a receiver,
//! publish environment or path authority, or manufacture a fallback shell.
//!
//! Every identity is fenced by the exact pane process generation, process id,
//! and kernel start token observed for that pane. A pid replacement detected
//! while resolving or using the evidence invalidates it, the runtime allows
//! exactly one bounded refresh per shell-interaction epoch, and a still
//! unusable identity settles permanently unknown with a precise diagnostic.

use super::{EnvironmentSignature, RuntimeSessionService, ShellClassification};
use std::path::{Path, PathBuf};

/// Which live process supplied OS identity evidence for a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimePaneProcessRole {
    /// The adapter-owned root process for the pane.
    AdapterOwnedRoot,
    /// The adapter-observed foreground process-group leader.
    ForegroundProcessGroupLeader,
}

impl RuntimePaneProcessRole {
    /// Returns the stable diagnostic label for this role.
    #[allow(dead_code)]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::AdapterOwnedRoot => "adapter-owned-root",
            Self::ForegroundProcessGroupLeader => "foreground-process-group-leader",
        }
    }
}

/// One OS-verified process instance resolving a pane's executable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneProcessIdentity {
    /// Which live pane process supplied the evidence.
    pub(crate) role: RuntimePaneProcessRole,
    /// Adapter-owned process generation when the pane is handed to an adapter.
    pub(crate) generation: Option<u64>,
    /// Process id that produced both observations.
    pub(crate) process_id: u32,
    /// Kernel start token observed with the executable path.
    pub(crate) start_token: u64,
    /// Absolute executable path read from the host kernel.
    pub(crate) executable_path: PathBuf,
}

/// Precise reason a pane has no usable OS process identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimePaneProcessIdentityUnavailable {
    /// The pane has no live process anymore.
    ProcessUnavailable,
    /// The observed process was replaced between the path and token reads.
    StartTokenChanged,
    /// The host exposed no executable metadata for the observed process.
    ExecutableUnreadable,
}

/// Maps one host process-instance observation failure onto pane identity state.
fn runtime_pane_process_identity_unavailable(
    reason: mez_mux::process::ProcessInstanceIdentityUnavailable,
) -> RuntimePaneProcessIdentityUnavailable {
    match reason {
        mez_mux::process::ProcessInstanceIdentityUnavailable::StartTokenChanged => {
            RuntimePaneProcessIdentityUnavailable::StartTokenChanged
        }
        mez_mux::process::ProcessInstanceIdentityUnavailable::Unreadable => {
            RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable
        }
    }
}

/// Stable reason a pane shell identity settled permanently unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeShellIdentityUnknownReason {
    /// No live pane process could be observed.
    ProcessUnavailable,
    /// The observed pane process was replaced while its identity was read.
    StartTokenChanged,
    /// The host exposed no executable metadata for the pane process.
    #[allow(dead_code)]
    ExecutableUnreadable,
    /// The executable name has no supported dialect and no attested dialect.
    UnrecognizedExecutable,
    /// The identity frame reported no usable dialect hint.
    DialectHintMissing,
    /// The identity frame reported no absolute launch target.
    LaunchTargetMissing,
    /// The identity probe produced no complete identity frame.
    IdentityFrameMissing,
    /// The reported dialect has no pane-mode bootstrap adapter.
    UnsupportedShell,
}

impl RuntimeShellIdentityUnknownReason {
    /// Returns the stable diagnostic label for this reason.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProcessUnavailable => "process_unavailable",
            Self::StartTokenChanged => "start_token_changed",
            Self::ExecutableUnreadable => "executable_unreadable",
            Self::UnrecognizedExecutable => "unrecognized_executable",
            Self::DialectHintMissing => "dialect_hint_missing",
            Self::LaunchTargetMissing => "launch_target_missing",
            Self::IdentityFrameMissing => "identity_frame_missing",
            Self::UnsupportedShell => "unsupported_shell",
        }
    }

    /// Returns the human-readable pre-dispatch diagnostic for this reason.
    pub(crate) fn diagnostic(self) -> &'static str {
        match self {
            Self::ProcessUnavailable => {
                "the pane shell process is unavailable, so no shell identity could be verified"
            }
            Self::StartTokenChanged => {
                "the pane shell process was replaced while its identity was read"
            }
            Self::ExecutableUnreadable => {
                "the host exposed no executable identity for the pane shell process"
            }
            Self::UnrecognizedExecutable => {
                "the pane shell executable has no supported dialect and no authenticated receiver"
            }
            Self::DialectHintMissing => {
                "the pane reported no usable shell dialect and no authenticated receiver"
            }
            Self::LaunchTargetMissing => {
                "the pane reported no absolute launch target and no verified local executable"
            }
            Self::IdentityFrameMissing => {
                "the pane shell identity probe produced no complete identity frame"
            }
            Self::UnsupportedShell => {
                "the pane shell dialect has no pane-mode bootstrap adapter; select native shell mode"
            }
        }
    }
}

/// Provenance of one shell dialect classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeShellDialectProvenance {
    /// Classified from an OS-verified executable for the pane's own process.
    OsProcessExecutable,
    /// Supplied by the runtime's authenticated managed receiver handshake.
    ManagedReceiver,
    /// Classified from the runtime's immutable pane spawn record.
    SessionSpawnRecorded,
    /// Reported only by an in-band frame or probe hint.
    InBandCorrelation,
}

/// Dialect evidence paired with its provenance and unknown reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimeShellDialectEvidence {
    /// Selected dialect, when one is available.
    pub(crate) classification: Option<ShellClassification>,
    /// How the dialect was established.
    pub(crate) provenance: RuntimeShellDialectProvenance,
    /// Typed reason the dialect stays unknown, when it does.
    pub(crate) unknown_reason: Option<RuntimeShellIdentityUnknownReason>,
}

impl RuntimeShellDialectEvidence {
    /// Builds dialect evidence classified from an OS-verified executable.
    fn os_process(classification: ShellClassification) -> Self {
        Self {
            classification: Some(classification),
            provenance: RuntimeShellDialectProvenance::OsProcessExecutable,
            unknown_reason: None,
        }
    }

    /// Builds dialect evidence classified from the pane spawn record.
    fn session_spawn_recorded(classification: ShellClassification) -> Self {
        Self {
            classification: Some(classification),
            provenance: RuntimeShellDialectProvenance::SessionSpawnRecorded,
            unknown_reason: None,
        }
    }

    /// Builds dialect evidence supplied by an authenticated managed receiver.
    fn managed_receiver(classification: ShellClassification) -> Self {
        Self {
            classification: Some(classification),
            provenance: RuntimeShellDialectProvenance::ManagedReceiver,
            unknown_reason: None,
        }
    }

    /// Builds correlation-only dialect evidence from an in-band hint.
    fn in_band(classification: Option<ShellClassification>) -> Self {
        Self {
            classification,
            provenance: RuntimeShellDialectProvenance::InBandCorrelation,
            unknown_reason: classification
                .is_none()
                .then_some(RuntimeShellIdentityUnknownReason::UnrecognizedExecutable),
        }
    }

    /// Builds typed unknown dialect evidence with a stable reason.
    fn unknown(reason: RuntimeShellIdentityUnknownReason) -> Self {
        Self {
            classification: None,
            provenance: RuntimeShellDialectProvenance::InBandCorrelation,
            unknown_reason: Some(reason),
        }
    }
}

/// Executable path evidence for one pane shell identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeShellExecutableEvidence {
    /// Absolute executable read from the host kernel for the pane process.
    OsProcessOwner {
        /// Role of the observed process inside the pane.
        role: RuntimePaneProcessRole,
        /// Process id that produced the observation.
        process_id: u32,
        /// Kernel start token observed with the path.
        start_token: u64,
        /// Adapter-owned process generation, when detached.
        generation: Option<u64>,
        /// Absolute executable path.
        path: PathBuf,
    },
    /// Executable recorded by the runtime when it launched the pane process.
    SessionSpawnRecorded {
        /// Absolute executable the runtime launched into the pane.
        path: PathBuf,
    },
    /// Absolute launch target reported in-band by the pane itself.
    InBandCorrelation {
        /// Self-reported absolute launch target; never executed or resolved.
        path: PathBuf,
    },
    /// No usable executable evidence exists.
    #[allow(dead_code)]
    Unknown {
        /// Typed reason the path stays unknown.
        reason: RuntimeShellIdentityUnknownReason,
    },
}

impl RuntimeShellExecutableEvidence {
    /// Returns the usable absolute path for this evidence, when it has one.
    pub(crate) fn path(&self) -> Option<&Path> {
        match self {
            Self::OsProcessOwner { path, .. }
            | Self::SessionSpawnRecorded { path }
            | Self::InBandCorrelation { path } => Some(path.as_path()),
            Self::Unknown { .. } => None,
        }
    }

    /// Reports whether the executable was read from the host kernel.
    #[allow(dead_code)]
    pub(crate) fn is_os_process_owner(&self) -> bool {
        matches!(self, Self::OsProcessOwner { .. })
    }

    /// Reports whether the executable came only from an in-band record.
    pub(crate) fn is_in_band_correlation(&self) -> bool {
        matches!(self, Self::InBandCorrelation { .. })
    }

    /// Returns the typed unknown reason, when the path is unusable.
    #[allow(dead_code)]
    fn unknown_reason(&self) -> Option<RuntimeShellIdentityUnknownReason> {
        match self {
            Self::Unknown { reason } => Some(*reason),
            _ => None,
        }
    }
}

/// Attestation owned by one pane shell identity.
///
/// This state is produced only by the certification owner. No executable path
/// evidence can construct it, so a reported or discovered path can never
/// promote itself into an authenticated receiver or published authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeShellAttestation {
    /// No authenticated managed receiver owns this pane.
    Unattested,
    /// The runtime's authenticated managed handshake installed the receiver.
    ManagedReceiver {
        /// Dialect the receiver was installed for.
        dialect: ShellClassification,
    },
}

impl RuntimeShellAttestation {
    /// Reports whether an authenticated managed receiver owns this pane.
    pub(crate) fn is_managed_receiver(self) -> bool {
        matches!(self, Self::ManagedReceiver { .. })
    }
}

/// Separately typed dialect, executable, and attestation evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneShellIdentityEvidence {
    /// Dialect plus provenance and unknown reason.
    pub(crate) dialect: RuntimeShellDialectEvidence,
    /// Executable path evidence.
    pub(crate) executable: RuntimeShellExecutableEvidence,
    /// Receiver attestation; never derived from executable evidence.
    pub(crate) attestation: RuntimeShellAttestation,
}

impl RuntimePaneShellIdentityEvidence {
    /// Builds evidence from an OS-verified pane process identity.
    pub(crate) fn from_os_process(identity: &RuntimePaneProcessIdentity) -> Self {
        let classification = ShellClassification::classify(&identity.executable_path);
        let dialect = if classification != ShellClassification::UnknownUnix {
            RuntimeShellDialectEvidence::os_process(classification)
        } else {
            RuntimeShellDialectEvidence::unknown(
                RuntimeShellIdentityUnknownReason::UnrecognizedExecutable,
            )
        };
        Self {
            dialect,
            executable: RuntimeShellExecutableEvidence::OsProcessOwner {
                role: identity.role,
                process_id: identity.process_id,
                start_token: identity.start_token,
                generation: identity.generation,
                path: identity.executable_path.clone(),
            },
            attestation: RuntimeShellAttestation::Unattested,
        }
    }

    /// Builds correlation evidence from the runtime's immutable spawn record.
    pub(crate) fn from_session_spawn_record(
        path: PathBuf,
        classification: ShellClassification,
    ) -> Self {
        let dialect = if classification == ShellClassification::UnknownUnix {
            RuntimeShellDialectEvidence::unknown(
                RuntimeShellIdentityUnknownReason::UnrecognizedExecutable,
            )
        } else {
            RuntimeShellDialectEvidence::session_spawn_recorded(classification)
        };
        Self {
            dialect,
            executable: RuntimeShellExecutableEvidence::SessionSpawnRecorded { path },
            attestation: RuntimeShellAttestation::Unattested,
        }
    }

    /// Builds correlation-only evidence from an in-band probe frame.
    pub(crate) fn from_in_band_launch_target(
        path: PathBuf,
        dialect_hint: Option<ShellClassification>,
    ) -> Self {
        Self {
            dialect: RuntimeShellDialectEvidence::in_band(dialect_hint),
            executable: RuntimeShellExecutableEvidence::InBandCorrelation { path },
            attestation: RuntimeShellAttestation::Unattested,
        }
    }

    /// Builds typed unknown evidence with a stable reason.
    #[allow(dead_code)]
    pub(crate) fn unknown(reason: RuntimeShellIdentityUnknownReason) -> Self {
        Self {
            dialect: RuntimeShellDialectEvidence::unknown(reason),
            executable: RuntimeShellExecutableEvidence::Unknown { reason },
            attestation: RuntimeShellAttestation::Unattested,
        }
    }

    /// Returns the authenticated dialect, preferring OS-verified evidence.
    pub(crate) fn effective_dialect(&self) -> Option<ShellClassification> {
        self.dialect.classification
    }

    /// Marks this evidence as owned by an authenticated managed receiver.
    pub(crate) fn with_managed_receiver_attestation(
        mut self,
        dialect: ShellClassification,
    ) -> Self {
        if matches!(
            self.dialect.provenance,
            RuntimeShellDialectProvenance::InBandCorrelation
        ) {
            self.dialect = RuntimeShellDialectEvidence::managed_receiver(dialect);
        }
        self.attestation = RuntimeShellAttestation::ManagedReceiver { dialect };
        self
    }

    /// Reports whether this identity may publish environment or path authority.
    ///
    /// Both an authenticated receiver and a dialect verified against an
    /// OS-owned process or the runtime's immutable spawn record are required.
    /// In-band launch targets can render a staged child but never authorize a
    /// pane, and an attested dialect over an unrecognized wrapper executable
    /// keeps its dialect without publishing path authority.
    pub(crate) fn publishes_executable_path_authority(&self) -> bool {
        self.attestation.is_managed_receiver()
            && !self.executable.is_in_band_correlation()
            && self.dialect.classification.is_some()
            && matches!(
                self.dialect.provenance,
                RuntimeShellDialectProvenance::OsProcessExecutable
                    | RuntimeShellDialectProvenance::SessionSpawnRecorded
            )
    }

    /// Returns a precise pre-dispatch diagnostic for this evidence.
    #[allow(dead_code)]
    pub(crate) fn unknown_diagnostic(&self) -> Option<&'static str> {
        self.dialect
            .unknown_reason
            .map(|reason| reason.diagnostic())
    }
}

/// Settled typed shell identity failure for one pane interaction epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimePaneShellIdentityUnknown {
    /// Stable reason the epoch settled unknown.
    pub(crate) reason: RuntimeShellIdentityUnknownReason,
    /// Interaction generation the failure belongs to.
    pub(crate) interaction_generation: Option<u64>,
}

/// Bounded refresh bookkeeping for one pane interaction epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RuntimePaneShellIdentityRefresh {
    /// Interaction generation the refresh budget belongs to.
    interaction_generation: Option<u64>,
    /// Whether the single permitted refresh was already consumed.
    used: bool,
}

/// Test-only injected OS process identity outcomes for one pane.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct RuntimePaneProcessIdentityInjections {
    outcomes: Vec<RuntimePaneProcessIdentityInjection>,
    next: std::cell::Cell<usize>,
}

#[cfg(test)]
impl RuntimePaneProcessIdentityInjections {
    /// Appends one injected outcome consumed by the next resolution attempt.
    pub(crate) fn push(&mut self, outcome: RuntimePaneProcessIdentityInjection) {
        self.outcomes.push(outcome);
    }

    /// Returns the next injected outcome for this pane, if any remain.
    pub(crate) fn next(&self) -> Option<&RuntimePaneProcessIdentityInjection> {
        let index = self.next.get();
        let outcome = self.outcomes.get(index)?;
        self.next.set(index.saturating_add(1));
        Some(outcome)
    }
}

/// One deterministic OS identity outcome injected by a test.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimePaneProcessIdentityInjection {
    /// An identity that must be verified against an independently observed
    /// live start token. `None` means the injected token is current.
    Identity {
        /// Role reported for the observed process.
        role: RuntimePaneProcessRole,
        /// Adapter-owned generation reported for the observed process.
        generation: Option<u64>,
        /// Process id reported for the observed process.
        process_id: u32,
        /// Start token reported for the observed process.
        start_token: u64,
        /// Absolute executable path reported for the observed process.
        executable_path: PathBuf,
        /// Start token the live re-read must observe; `None` accepts the
        /// injected token as current.
        live_start_token: Option<u64>,
    },
    /// No OS identity is available for the observed process.
    Unavailable(RuntimePaneProcessIdentityUnavailable),
}

impl RuntimeSessionService {
    /// Injects one deterministic OS identity outcome for a pane test.
    #[cfg(test)]
    pub(crate) fn inject_pane_process_identity_for_tests(
        &mut self,
        pane_id: &str,
        outcome: RuntimePaneProcessIdentityInjection,
    ) {
        self.process
            .pane_process_identity_injections
            .entry(pane_id.to_string())
            .or_default()
            .push(outcome);
    }

    /// Installs one sticky OS-verified pane executable for a pane test.
    ///
    /// Unlike the one-shot outcome queue, this override resolves every
    /// subsequent identity read for the pane, so a test can declare the
    /// pane's OS-verified dialect without a live shell process of that
    /// dialect.
    #[cfg(test)]
    pub(crate) fn set_pane_process_executable_for_tests(
        &mut self,
        pane_id: &str,
        executable_path: impl Into<PathBuf>,
    ) {
        let identity = RuntimePaneProcessIdentity {
            role: RuntimePaneProcessRole::AdapterOwnedRoot,
            generation: None,
            process_id: self.primary_pid_for_live_pane_process(pane_id).unwrap_or(0),
            start_token: 0,
            executable_path: executable_path.into(),
        };
        self.process
            .pane_process_identity_overrides
            .borrow_mut()
            .insert(pane_id.to_string(), identity);
    }

    /// Returns one OS-verified process identity for a pane.
    ///
    /// The adapter-owned root process is preferred; when it cannot be read the
    /// pane's observed foreground process-group leader is used, which is how a
    /// local foreign shell (for example `zsh` started inside a `bash` pane)
    /// receives OS-verified executable evidence.
    pub(crate) fn pane_process_identity(
        &self,
        pane_id: &str,
    ) -> Result<RuntimePaneProcessIdentity, RuntimePaneProcessIdentityUnavailable> {
        #[cfg(test)]
        if let Some(outcome) = self
            .process
            .pane_process_identity_injections
            .get(pane_id)
            .and_then(RuntimePaneProcessIdentityInjections::next)
        {
            return match outcome {
                RuntimePaneProcessIdentityInjection::Identity {
                    role,
                    generation,
                    process_id,
                    start_token,
                    executable_path,
                    live_start_token,
                } => {
                    if live_start_token.is_some_and(|token| token != *start_token) {
                        return Err(RuntimePaneProcessIdentityUnavailable::StartTokenChanged);
                    }
                    let identity = RuntimePaneProcessIdentity {
                        role: *role,
                        generation: *generation,
                        process_id: *process_id,
                        start_token: *start_token,
                        executable_path: executable_path.clone(),
                    };
                    self.pin_pane_process_identity_for_tests(pane_id, &identity);
                    Ok(identity)
                }
                RuntimePaneProcessIdentityInjection::Unavailable(reason) => Err(*reason),
            };
        }

        #[cfg(test)]
        if let Some(identity) = self
            .process
            .pane_process_identity_overrides
            .borrow()
            .get(pane_id)
            .cloned()
        {
            self.pin_pane_process_identity_for_tests(pane_id, &identity);
            return Ok(identity);
        }

        let generation = self
            .adapter_owned_pane_process_instance(pane_id)
            .map(|instance| instance.generation);
        let primary_process_id = self
            .primary_pid_for_live_pane_process(pane_id)
            .ok_or(RuntimePaneProcessIdentityUnavailable::ProcessUnavailable)?;
        match mez_mux::process::process_executable_identity_for_pid(primary_process_id) {
            Ok(identity) => {
                let identity = RuntimePaneProcessIdentity {
                    role: RuntimePaneProcessRole::AdapterOwnedRoot,
                    generation,
                    process_id: identity.process_id,
                    start_token: identity.start_token,
                    executable_path: identity.executable_path,
                };
                #[cfg(test)]
                self.pin_pane_process_identity_for_tests(pane_id, &identity);
                return Ok(identity);
            }
            Err(mez_mux::process::ProcessInstanceIdentityUnavailable::StartTokenChanged) => {
                // A pid replacement invalidates the whole observation: never
                // pair one instance's executable with a replacement's lifetime
                // or degrade the typed replacement reason to "unreadable".
                return Err(RuntimePaneProcessIdentityUnavailable::StartTokenChanged);
            }
            Err(mez_mux::process::ProcessInstanceIdentityUnavailable::Unreadable) => {}
        }
        let (foreground_group, _) = self.pane_foreground_process_group_observation(pane_id);
        if let Some(leader) = foreground_group.filter(|leader| *leader != primary_process_id) {
            match mez_mux::process::process_executable_identity_for_pid(leader) {
                Ok(identity) => {
                    let identity = RuntimePaneProcessIdentity {
                        role: RuntimePaneProcessRole::ForegroundProcessGroupLeader,
                        generation,
                        process_id: identity.process_id,
                        start_token: identity.start_token,
                        executable_path: identity.executable_path,
                    };
                    #[cfg(test)]
                    self.pin_pane_process_identity_for_tests(pane_id, &identity);
                    return Ok(identity);
                }
                Err(reason) => return Err(runtime_pane_process_identity_unavailable(reason)),
            }
        }
        Err(RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable)
    }

    /// Records one verified OS identity for test-only staleness checks.
    #[cfg(test)]
    fn pin_pane_process_identity_for_tests(
        &self,
        pane_id: &str,
        identity: &RuntimePaneProcessIdentity,
    ) {
        self.process.pane_process_identity_pins.borrow_mut().insert(
            pane_id.to_string(),
            (
                identity.process_id,
                identity.start_token,
                identity.executable_path.clone(),
            ),
        );
    }

    /// Verifies identity evidence against the live pane process before use.
    ///
    /// OS-owned evidence must still describe the current process id, start
    /// token, and adapter-owned generation. Correlation and spawn-record
    /// evidence carry no OS claim, so they are fenced by the interaction epoch
    /// at their construction sites instead.
    pub(crate) fn pane_shell_identity_evidence_is_current(
        &self,
        pane_id: &str,
        evidence: &RuntimePaneShellIdentityEvidence,
    ) -> bool {
        let RuntimeShellExecutableEvidence::OsProcessOwner {
            role,
            process_id,
            start_token,
            generation,
            path,
        } = &evidence.executable
        else {
            return true;
        };
        #[cfg(test)]
        if let Some(pin) = self
            .process
            .pane_process_identity_pins
            .borrow()
            .get(pane_id)
            && pin.0 == *process_id
            && pin.1 == *start_token
            && pin.2 == *path
        {
            let live_generation = self
                .adapter_owned_pane_process_instance(pane_id)
                .map(|instance| instance.generation);
            return generation.is_none() || *generation == live_generation;
        }
        let identity = RuntimePaneProcessIdentity {
            role: *role,
            generation: *generation,
            process_id: *process_id,
            start_token: *start_token,
            executable_path: path.clone(),
        };
        self.pane_process_identity_is_current(pane_id, &identity)
    }

    /// Reports whether OS identity evidence still describes the live pane process.
    pub(crate) fn pane_process_identity_is_current(
        &self,
        pane_id: &str,
        identity: &RuntimePaneProcessIdentity,
    ) -> bool {
        let generation = self
            .adapter_owned_pane_process_instance(pane_id)
            .map(|instance| instance.generation);
        // Evidence captured before adapter handoff carries no generation; the
        // pid and start-token checks below still fence the exact process. Two
        // known generations must match so a replaced pane instance is rejected.
        if identity.generation.is_some() && identity.generation != generation {
            return false;
        }
        let primary_process_id = self.primary_pid_for_live_pane_process(pane_id);
        let process_is_observed = primary_process_id == Some(identity.process_id) || {
            let (foreground_group, _) = self.pane_foreground_process_group_observation(pane_id);
            foreground_group == Some(identity.process_id)
        };
        process_is_observed
            && mez_mux::process::process_start_token_for_pid(identity.process_id)
                == Some(identity.start_token)
    }

    /// Resolves identity evidence from an OS-observed pane process and the
    /// runtime's own spawn record.
    pub(crate) fn session_shell_identity_evidence(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
    ) -> Result<RuntimePaneShellIdentityEvidence, RuntimeShellIdentityUnknownReason> {
        self.resolve_shell_identity_evidence_with_bounded_refresh(
            pane_id,
            interaction_generation,
            |service| {
                let live = service.pane_process_identity(pane_id);
                if let Ok(identity) = live.as_ref()
                    && let classification = ShellClassification::classify(&identity.executable_path)
                    && classification != ShellClassification::UnknownUnix
                {
                    return Ok(RuntimePaneShellIdentityEvidence::from_os_process(identity));
                }
                // A replaced pane process invalidates every identity derived
                // from it; a stale spawn record must never mask that.
                if matches!(
                    live.as_ref().err(),
                    Some(RuntimePaneProcessIdentityUnavailable::StartTokenChanged)
                ) {
                    return Err(RuntimeShellIdentityUnknownReason::StartTokenChanged);
                }
                // The live executable is not a recognized shell. Fall back to
                // the runtime's immutable spawn record only when its name is a
                // supported dialect; a renamed or wrapper shell is typed
                // unknown instead of being promoted by version text.
                let session_path = service.session.shell.path().to_path_buf();
                let classification = ShellClassification::classify(&session_path);
                if classification != ShellClassification::UnknownUnix {
                    return Ok(RuntimePaneShellIdentityEvidence::from_session_spawn_record(
                        session_path,
                        classification,
                    ));
                }
                Err(match live {
                    Err(RuntimePaneProcessIdentityUnavailable::ProcessUnavailable) => {
                        RuntimeShellIdentityUnknownReason::ProcessUnavailable
                    }
                    Err(RuntimePaneProcessIdentityUnavailable::StartTokenChanged) => {
                        RuntimeShellIdentityUnknownReason::StartTokenChanged
                    }
                    Err(RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable) | Ok(_) => {
                        RuntimeShellIdentityUnknownReason::UnrecognizedExecutable
                    }
                })
            },
        )
    }

    /// Resolves identity evidence from one syntax-neutral identity probe.
    pub(crate) fn probe_shell_identity_evidence(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
        dialect_hint: Option<ShellClassification>,
        launch_hint: Option<PathBuf>,
    ) -> Result<RuntimePaneShellIdentityEvidence, RuntimeShellIdentityUnknownReason> {
        self.resolve_shell_identity_evidence_with_bounded_refresh(
            pane_id,
            interaction_generation,
            |service| {
                let live = service.pane_process_identity(pane_id);
                if let Ok(identity) = live.as_ref()
                    && let classification = ShellClassification::classify(&identity.executable_path)
                    && classification != ShellClassification::UnknownUnix
                {
                    return Ok(RuntimePaneShellIdentityEvidence::from_os_process(identity));
                }
                let Some(launch_target) = launch_hint.clone() else {
                    return Err(match live {
                        Err(RuntimePaneProcessIdentityUnavailable::ProcessUnavailable) => {
                            RuntimeShellIdentityUnknownReason::ProcessUnavailable
                        }
                        Err(RuntimePaneProcessIdentityUnavailable::StartTokenChanged) => {
                            RuntimeShellIdentityUnknownReason::StartTokenChanged
                        }
                        Err(RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable)
                        | Ok(_) => RuntimeShellIdentityUnknownReason::LaunchTargetMissing,
                    });
                };
                if dialect_hint.is_none() {
                    return Err(RuntimeShellIdentityUnknownReason::UnsupportedShell);
                }
                Ok(
                    RuntimePaneShellIdentityEvidence::from_in_band_launch_target(
                        launch_target,
                        dialect_hint,
                    ),
                )
            },
        )
    }

    /// Resolves evidence with exactly one bounded refresh, then settles unknown.
    fn resolve_shell_identity_evidence_with_bounded_refresh<F>(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
        mut attempt: F,
    ) -> Result<RuntimePaneShellIdentityEvidence, RuntimeShellIdentityUnknownReason>
    where
        F: FnMut(
            &Self,
        )
            -> Result<RuntimePaneShellIdentityEvidence, RuntimeShellIdentityUnknownReason>,
    {
        self.reset_pane_shell_identity_epoch_state(pane_id, interaction_generation);
        if let Some(unknown) =
            self.settled_pane_shell_identity_unknown(pane_id, interaction_generation)
        {
            return Err(unknown.reason);
        }
        match attempt(self) {
            Ok(evidence) => return Ok(evidence),
            Err(reason) => {
                if !self.consume_pane_shell_identity_refresh(pane_id, interaction_generation) {
                    self.settle_pane_shell_identity_unknown(
                        pane_id,
                        interaction_generation,
                        reason,
                    );
                    return Err(reason);
                }
            }
        }
        match attempt(self) {
            Ok(evidence) => Ok(evidence),
            Err(reason) => {
                self.settle_pane_shell_identity_unknown(pane_id, interaction_generation, reason);
                Err(reason)
            }
        }
    }

    /// Clears settled failures and refresh budgets from older epochs.
    fn reset_pane_shell_identity_epoch_state(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
    ) {
        let stale_unknown = self
            .process
            .pane_shell_identity_unknowns
            .borrow()
            .get(pane_id)
            .is_some_and(|unknown| unknown.interaction_generation != interaction_generation);
        if stale_unknown {
            self.process
                .pane_shell_identity_unknowns
                .borrow_mut()
                .remove(pane_id);
        }
        let stale_refresh = self
            .process
            .pane_shell_identity_refreshes
            .borrow()
            .get(pane_id)
            .is_some_and(|refresh| refresh.interaction_generation != interaction_generation);
        if stale_refresh {
            self.process
                .pane_shell_identity_refreshes
                .borrow_mut()
                .remove(pane_id);
        }
    }

    /// Returns one settled identity failure recorded for `interaction_generation`.
    ///
    /// Settlement is scoped to the epoch that observed it: a failure recorded
    /// for an older generation must never gate a fresh shell-interaction epoch.
    pub(crate) fn settled_pane_shell_identity_unknown(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
    ) -> Option<RuntimePaneShellIdentityUnknown> {
        self.process
            .pane_shell_identity_unknowns
            .borrow()
            .get(pane_id)
            .filter(|unknown| unknown.interaction_generation == interaction_generation)
            .cloned()
    }

    /// Records or consumes the one bounded refresh for this pane epoch.
    fn consume_pane_shell_identity_refresh(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
    ) -> bool {
        let mut refreshes = self.process.pane_shell_identity_refreshes.borrow_mut();
        match refreshes.get_mut(pane_id) {
            Some(refresh) if refresh.interaction_generation == interaction_generation => {
                if refresh.used {
                    false
                } else {
                    refresh.used = true;
                    true
                }
            }
            Some(refresh) => {
                *refresh = RuntimePaneShellIdentityRefresh {
                    interaction_generation,
                    used: true,
                };
                true
            }
            None => {
                refreshes.insert(
                    pane_id.to_string(),
                    RuntimePaneShellIdentityRefresh {
                        interaction_generation,
                        used: true,
                    },
                );
                true
            }
        }
    }

    /// Settles one permanent typed failure for the current pane epoch.
    fn settle_pane_shell_identity_unknown(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
        reason: RuntimeShellIdentityUnknownReason,
    ) {
        self.process
            .pane_shell_identity_unknowns
            .borrow_mut()
            .insert(
                pane_id.to_string(),
                RuntimePaneShellIdentityUnknown {
                    reason,
                    interaction_generation,
                },
            );
    }

    /// Settles one permanent typed identity failure for the current epoch.
    ///
    /// Probe settlement calls this for malformed or incomplete frames so the
    /// epoch can never be re-probed from deep observation frames.
    pub(crate) fn settle_pane_shell_identity_unknown_for_epoch(
        &self,
        pane_id: &str,
        interaction_generation: Option<u64>,
        reason: RuntimeShellIdentityUnknownReason,
    ) {
        self.settle_pane_shell_identity_unknown(pane_id, interaction_generation, reason);
    }

    /// Returns the runtime-managed dialect the pane was authenticated with.
    ///
    /// Only runtime-owned state contributes: the managed handoff adapter the
    /// runtime installed, or the OS-verified executable of the pane process.
    /// In-band frames never select an attested dialect.
    pub(crate) fn managed_receiver_dialect_for_pane(
        &self,
        pane_id: &str,
        evidence: &RuntimePaneShellIdentityEvidence,
    ) -> Option<ShellClassification> {
        if let Some(handoff) = self.process.pane_managed_shell_handoffs.get(pane_id) {
            return Some(match handoff.shell() {
                super::ManagedShellKind::Bash => ShellClassification::Bash,
                super::ManagedShellKind::Fish => ShellClassification::Fish,
                super::ManagedShellKind::Zsh => ShellClassification::Zsh,
            });
        }
        if evidence.dialect.provenance == RuntimeShellDialectProvenance::OsProcessExecutable {
            return evidence.dialect.classification;
        }
        self.process
            .pane_posix_compatibility
            .contains_key(pane_id)
            .then_some(ShellClassification::PosixSh)
    }

    /// Builds the certified identity evidence for one pane bootstrap.
    ///
    /// OS evidence is preferred. Without a live process, the pane's reported
    /// environment path is accepted as the runtime's own spawn record only
    /// when it equals the runtime's spawn record; any other absolute in-band
    /// path stays correlation-only. The managed receiver attestation is added
    /// only for a non-dependency-free handshake, and the attested dialect
    /// comes from runtime-owned receiver state or the evidence's own dialect.
    pub(crate) fn certified_pane_shell_identity_evidence(
        &self,
        pane_id: &str,
        signature: &EnvironmentSignature,
    ) -> RuntimePaneShellIdentityEvidence {
        let mut evidence = match self.pane_process_identity(pane_id) {
            Ok(identity) => RuntimePaneShellIdentityEvidence::from_os_process(&identity),
            Err(_) => {
                let session_path = self.session.shell.path();
                let signature_path = PathBuf::from(&signature.shell_path);
                if signature_path.is_absolute() && signature_path == session_path {
                    RuntimePaneShellIdentityEvidence::from_session_spawn_record(
                        signature_path.clone(),
                        ShellClassification::classify(&signature_path),
                    )
                } else if signature_path.is_absolute() {
                    RuntimePaneShellIdentityEvidence::from_in_band_launch_target(
                        signature_path,
                        classification_for_signature_path(signature),
                    )
                } else {
                    let session_path = session_path.to_path_buf();
                    RuntimePaneShellIdentityEvidence::from_session_spawn_record(
                        session_path.clone(),
                        ShellClassification::classify(&session_path),
                    )
                }
            }
        };
        if !self.pane_has_dependency_free_handoff(pane_id)
            && let Some(dialect) = self
                .managed_receiver_dialect_for_pane(pane_id, &evidence)
                .or(evidence.effective_dialect())
        {
            evidence = evidence.with_managed_receiver_attestation(dialect);
        }
        evidence
    }

    /// Returns the dialect usable for the managed-receiver wrap gate.
    ///
    /// Wrapping generated input in a private receiver payload is allowed only
    /// when the runtime installed that receiver for the pane and the live pane
    /// process executable verifies the dialect. An in-band classification or
    /// reported version never selects the private receiver transport.
    pub(crate) fn managed_receiver_wrap_dialect_for_pane(
        &self,
        pane_id: &str,
    ) -> Option<ShellClassification> {
        let managed_receiver_installed = self.process.pane_bash_compatibility.contains_key(pane_id)
            || self.process.pane_zsh_compatibility.contains_key(pane_id)
            || self.process.pane_fish_compatibility.contains_key(pane_id)
            || self
                .process
                .pane_managed_shell_handoffs
                .contains_key(pane_id);
        if !managed_receiver_installed {
            return None;
        }
        self.pane_os_verified_dialect(pane_id).or_else(|| {
            self.process
                .pane_managed_shell_handoffs
                .get(pane_id)
                .map(|handoff| match handoff.shell() {
                    super::ManagedShellKind::Bash => ShellClassification::Bash,
                    super::ManagedShellKind::Fish => ShellClassification::Fish,
                    super::ManagedShellKind::Zsh => ShellClassification::Zsh,
                })
        })
    }

    /// Returns the OS-verified dialect of the live pane process, if any.
    ///
    /// This is a read-only host observation: it never records refresh state,
    /// never consults in-band frames, and never resolves a bare name.
    pub(crate) fn pane_os_verified_dialect(&self, pane_id: &str) -> Option<ShellClassification> {
        let primary_process_id = self.primary_pid_for_live_pane_process(pane_id)?;
        let executable_path =
            mez_mux::process::process_executable_path_for_pid(primary_process_id)?;
        let classification = ShellClassification::classify(&executable_path);
        (classification != ShellClassification::UnknownUnix).then_some(classification)
    }
}

impl super::RuntimePaneShellExecutionIdentity {
    /// Builds one atomically fenced execution identity from typed evidence.
    pub(crate) fn from_evidence(
        primary_process_id: Option<u32>,
        interaction_generation: Option<u64>,
        version_probe: Option<String>,
        evidence: RuntimePaneShellIdentityEvidence,
    ) -> Result<Self, RuntimeShellIdentityUnknownReason> {
        let shell_path = evidence
            .executable
            .path()
            .map(Path::to_path_buf)
            .ok_or(RuntimeShellIdentityUnknownReason::LaunchTargetMissing)?;
        let classification = evidence
            .effective_dialect()
            .ok_or(RuntimeShellIdentityUnknownReason::DialectHintMissing)?;
        super::validate_pane_shell_executable_path(&shell_path)
            .map_err(|_| RuntimeShellIdentityUnknownReason::LaunchTargetMissing)?;
        Ok(Self {
            shell_path,
            classification,
            version_probe,
            primary_process_id,
            interaction_generation,
            evidence: Box::new(evidence),
        })
    }
}

/// Reports whether one parsed signature still names a usable dialect.
pub(crate) fn classification_for_signature_path(
    signature: &EnvironmentSignature,
) -> Option<ShellClassification> {
    let classification = ShellClassification::classify(&signature.shell_path);
    (classification != ShellClassification::UnknownUnix).then_some(classification)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os_evidence(path: &str) -> RuntimePaneShellIdentityEvidence {
        RuntimePaneShellIdentityEvidence::from_os_process(&RuntimePaneProcessIdentity {
            role: RuntimePaneProcessRole::AdapterOwnedRoot,
            generation: Some(7),
            process_id: 4242,
            start_token: 99,
            executable_path: PathBuf::from(path),
        })
    }

    #[test]
    fn os_owned_shell_publishes_authority_only_with_attestation() {
        let unattested = os_evidence("/usr/bin/bash");
        assert_eq!(
            unattested.effective_dialect(),
            Some(ShellClassification::Bash)
        );
        assert!(!unattested.publishes_executable_path_authority());

        let attested = unattested.with_managed_receiver_attestation(ShellClassification::Bash);
        assert!(attested.publishes_executable_path_authority());
    }

    #[test]
    fn wrapper_executable_keeps_attested_dialect_without_path_authority() {
        let wrapper = os_evidence("/opt/tooling/wrapper");
        assert_eq!(wrapper.effective_dialect(), None);
        let attested = wrapper.with_managed_receiver_attestation(ShellClassification::Bash);
        assert_eq!(
            attested.effective_dialect(),
            Some(ShellClassification::Bash)
        );
        assert!(attested.attestation.is_managed_receiver());
        assert!(!attested.publishes_executable_path_authority());
    }

    #[test]
    fn in_band_launch_target_never_attests_or_publishes_authority() {
        let evidence = RuntimePaneShellIdentityEvidence::from_in_band_launch_target(
            PathBuf::from("/bin/bash"),
            Some(ShellClassification::Bash),
        );
        assert_eq!(
            evidence.effective_dialect(),
            Some(ShellClassification::Bash)
        );
        assert!(!evidence.attestation.is_managed_receiver());
        assert!(!evidence.publishes_executable_path_authority());
    }

    #[test]
    fn session_spawn_record_requires_attestation_for_authority() {
        let unattested = RuntimePaneShellIdentityEvidence::from_session_spawn_record(
            PathBuf::from("/bin/sh"),
            ShellClassification::PosixSh,
        );
        assert!(!unattested.publishes_executable_path_authority());
        let attested = unattested.with_managed_receiver_attestation(ShellClassification::PosixSh);
        assert!(attested.publishes_executable_path_authority());
    }

    #[test]
    fn unknown_evidence_reports_a_stable_reason() {
        let evidence = RuntimePaneShellIdentityEvidence::unknown(
            RuntimeShellIdentityUnknownReason::UnrecognizedExecutable,
        );
        assert_eq!(evidence.effective_dialect(), None);
        assert!(!evidence.publishes_executable_path_authority());
        assert_eq!(
            evidence.unknown_diagnostic(),
            Some(
                "the pane shell executable has no supported dialect and no authenticated receiver"
            )
        );
    }

    fn test_service() -> RuntimeSessionService {
        crate::test_support::runtime::RuntimeServiceFixture::new().build()
    }

    fn replaced_identity() -> RuntimePaneProcessIdentityInjection {
        RuntimePaneProcessIdentityInjection::Identity {
            role: RuntimePaneProcessRole::AdapterOwnedRoot,
            generation: None,
            process_id: 4242,
            start_token: 7,
            executable_path: PathBuf::from("/usr/bin/bash"),
            live_start_token: Some(99),
        }
    }

    fn bash_identity() -> RuntimePaneProcessIdentityInjection {
        RuntimePaneProcessIdentityInjection::Identity {
            role: RuntimePaneProcessRole::AdapterOwnedRoot,
            generation: None,
            process_id: 4242,
            start_token: 7,
            executable_path: PathBuf::from("/usr/bin/bash"),
            live_start_token: None,
        }
    }

    fn unavailable_identity() -> RuntimePaneProcessIdentityInjection {
        RuntimePaneProcessIdentityInjection::Unavailable(
            RuntimePaneProcessIdentityUnavailable::StartTokenChanged,
        )
    }

    #[test]
    fn injected_start_token_mismatch_is_rejected_and_settles_unknown() {
        let mut service = test_service();
        service.inject_pane_process_identity_for_tests("%9", replaced_identity());
        service.inject_pane_process_identity_for_tests("%9", replaced_identity());

        let reason = service
            .session_shell_identity_evidence("%9", Some(1))
            .expect_err("a replaced pid must never produce identity evidence");
        assert_eq!(reason, RuntimeShellIdentityUnknownReason::StartTokenChanged);
        let settled = service
            .settled_pane_shell_identity_unknown("%9", Some(1))
            .expect("the epoch must settle permanently unknown");
        assert_eq!(
            settled.reason,
            RuntimeShellIdentityUnknownReason::StartTokenChanged
        );
    }

    #[test]
    fn host_start_token_replacement_maps_to_the_typed_pane_reason() {
        assert_eq!(
            runtime_pane_process_identity_unavailable(
                mez_mux::process::ProcessInstanceIdentityUnavailable::StartTokenChanged
            ),
            RuntimePaneProcessIdentityUnavailable::StartTokenChanged
        );
        assert_eq!(
            runtime_pane_process_identity_unavailable(
                mez_mux::process::ProcessInstanceIdentityUnavailable::Unreadable
            ),
            RuntimePaneProcessIdentityUnavailable::ExecutableUnreadable
        );
    }

    #[test]
    fn one_transient_refresh_then_os_evidence_succeeds() {
        let mut service = test_service();
        service.inject_pane_process_identity_for_tests("%9", unavailable_identity());
        service.inject_pane_process_identity_for_tests("%9", bash_identity());

        let evidence = service
            .session_shell_identity_evidence("%9", Some(4))
            .expect("the single bounded refresh should succeed");
        assert_eq!(
            evidence.effective_dialect(),
            Some(ShellClassification::Bash)
        );
        assert!(evidence.executable.is_os_process_owner());
        assert!(
            service
                .settled_pane_shell_identity_unknown("%9", Some(4))
                .is_none()
        );
    }

    #[test]
    fn permanent_unknown_settles_within_the_refresh_budget() {
        let mut service = test_service();
        service.inject_pane_process_identity_for_tests("%9", unavailable_identity());
        service.inject_pane_process_identity_for_tests("%9", unavailable_identity());

        let reason = service
            .session_shell_identity_evidence("%9", Some(3))
            .unwrap_err();
        assert_eq!(reason, RuntimeShellIdentityUnknownReason::StartTokenChanged);
        assert!(
            service
                .settled_pane_shell_identity_unknown("%9", Some(3))
                .is_some()
        );
        // The settled epoch is terminal: no further refresh is consumed.
        assert_eq!(
            service
                .session_shell_identity_evidence("%9", Some(3))
                .unwrap_err(),
            RuntimeShellIdentityUnknownReason::StartTokenChanged
        );
    }

    #[test]
    fn new_interaction_epoch_reprobes_once_after_settled_unknown() {
        let mut service = test_service();
        service.inject_pane_process_identity_for_tests("%9", unavailable_identity());
        service.inject_pane_process_identity_for_tests("%9", unavailable_identity());

        let reason = service
            .session_shell_identity_evidence("%9", Some(1))
            .expect_err("the bounded refresh must settle the epoch unknown");
        assert_eq!(reason, RuntimeShellIdentityUnknownReason::StartTokenChanged);
        assert!(
            service
                .settled_pane_shell_identity_unknown("%9", Some(1))
                .is_some()
        );

        // The same epoch stays terminal: the injected success must never be
        // consumed by another probe attempt.
        service.inject_pane_process_identity_for_tests("%9", bash_identity());
        assert_eq!(
            service
                .session_shell_identity_evidence("%9", Some(1))
                .expect_err("the settled epoch must not re-probe"),
            RuntimeShellIdentityUnknownReason::StartTokenChanged
        );

        // A changed foreground shell begins a new interaction epoch. The
        // previous epoch's settled unknown must not gate it, and the new epoch
        // gets its own probe that can succeed.
        assert!(service.begin_uncertified_foreign_shell_boundary("%9", 4242, 4243));
        assert!(
            service
                .settled_pane_shell_identity_unknown("%9", Some(1))
                .is_none(),
            "an epoch transition must drop the previous epoch's settled unknown"
        );
        let current_generation = service
            .pane_shell_interaction_generation_for_tests("%9")
            .expect("the new boundary must record its interaction epoch");
        let evidence = service
            .session_shell_identity_evidence("%9", Some(current_generation))
            .expect("the new epoch must probe once and can succeed");
        assert_eq!(
            evidence.effective_dialect(),
            Some(ShellClassification::Bash)
        );
        assert!(evidence.executable.is_os_process_owner());
        assert!(
            service
                .settled_pane_shell_identity_unknown("%9", Some(current_generation))
                .is_none()
        );
    }

    #[test]
    fn missing_probe_frame_is_typed_unknown_without_a_manufactured_shell() {
        let mut service = test_service();
        let unrecognized = RuntimePaneProcessIdentityInjection::Identity {
            role: RuntimePaneProcessRole::AdapterOwnedRoot,
            generation: None,
            process_id: 4242,
            start_token: 7,
            executable_path: PathBuf::from("/usr/bin/ssh"),
            live_start_token: None,
        };
        service.inject_pane_process_identity_for_tests("%9", unrecognized.clone());
        service.inject_pane_process_identity_for_tests("%9", unrecognized);

        let reason = service
            .probe_shell_identity_evidence("%9", Some(5), None, None)
            .expect_err("no frame hints must never manufacture an identity");
        assert_eq!(
            reason,
            RuntimeShellIdentityUnknownReason::LaunchTargetMissing
        );
        assert!(
            service
                .settled_pane_shell_identity_unknown("%9", Some(5))
                .is_some()
        );

        let error = super::super::RuntimePaneShellExecutionIdentity::from_evidence(
            None,
            Some(5),
            None,
            RuntimePaneShellIdentityEvidence::unknown(
                RuntimeShellIdentityUnknownReason::IdentityFrameMissing,
            ),
        )
        .expect_err("typed unknown evidence must never build an executable identity");
        assert_eq!(
            error,
            RuntimeShellIdentityUnknownReason::LaunchTargetMissing
        );
    }
}
