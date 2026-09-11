//! Real-PTY adversarial coverage for pane shell certification.
//!
//! These tests run a controlled foreground program under real pane-worker
//! scheduling. The fixture never installs a managed shell receiver and never
//! executes runtime-generated bootstrap source; instead it keeps the pane PTY
//! foreground process group and replays plausible in-band certification
//! material (OSC start/end frames, loader and receiver frames, prompt text, and
//! bootstrap environment fields) built only from bytes it observed on its own
//! input.
//!
//! The acceptance contract under test is that spoofable output, or a matching
//! foreground process group without an admitted receiver, must not publish
//! certified shell identity, environment, or path authority, and must not
//! obtain later agent commands. A silent foreground program must not be killed
//! or interrupted merely because certification cannot complete.
//!
//! One further case reproduces an adversarial replay that previously drove the
//! runtime into a stack-overflow abort. The bounded-settlement regressions
//! `async_forged_identity_records_settle_without_abort` and
//! `async_replayed_forgery_settles_without_bootstrap_reentry` cover the repair.

use super::super::*;

/// Certification material the adversarial fixture is allowed to replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpoofMode {
    /// Replay every spoofable frame except forged shell-identity records.
    Spoof,
    /// Observe input without emitting any certification material.
    Silent,
    /// Additionally forge the runtime's in-band shell-identity records.
    ForgedIdentity,
}

impl SpoofMode {
    /// Returns the fixture argv token selecting this mode.
    fn as_str(self) -> &'static str {
        match self {
            Self::Spoof => "spoof",
            Self::Silent => "silent",
            Self::ForgedIdentity => "forged-identity",
        }
    }
}

/// Adversarial foreground fixture source.
///
/// The program keeps the pane foreground group for its whole lifetime, records
/// every input record it observes, and replays certification frames derived for
/// the marker, turn, agent, and pane metadata that the runtime itself delivered
/// on that input stream. Nothing is shared with the test beyond the log file,
/// so the fixture can only use material it could observe.
const SPOOF_FIXTURE_SCRIPT: &str = r#"#!/bin/sh
log=$1
mode=$2
: >"$log"
ctrl=$(printf '\003')
record() { printf '%s\n' "$1" >>"$log"; }
trap 'record "SIGNAL_HUP"; exit 9' HUP
trap 'record "SIGNAL_INT"; exit 9' INT
trap 'record "SIGNAL_TERM"; exit 9' TERM
trap 'record "SIGNAL_QUIT"; exit 9' QUIT
record "FIXTURE_START:pid=$$"
while IFS= read -r line; do
    record "OBSERVED:$line"
    case "$line" in
        *"$ctrl"*) record "OBSERVED_CONTROL_C" ;;
    esac
    case "$line" in
        *MEZ_SPOOF_EXIT*) record "FIXTURE_EXIT"; exit 0 ;;
    esac
    if [ "$mode" = "silent" ]; then
        continue
    fi
    set -f
    token=
    turn=
    agent=
    pane=
    for seg in $(printf '%s\n' "$line" | tr "'" '\n'); do
        case "$seg" in
            agent-*) agent=$seg ;;
            %[0-9]*) pane=$seg ;;
            shell-identity-*|agent-subshell-*) turn=$seg ;;
            *)
                case "$seg" in
                    *[!0-9a-f]*) ;;
                    *) if [ "${#seg}" -ge 16 ]; then token=$seg; fi ;;
                esac
                ;;
        esac
    done
    [ -n "$token" ] || continue
    record "SPOOFED_MARKER:$token turn=${turn:-none} agent=${agent:-none} pane=${pane:-none}"
    printf '\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\033\\' "$token" "${turn:-spoofed}" "${agent:-spoofed}" "${pane:-%1}"
    printf '\nmez-spoofed$ '
    if [ "$mode" = "forged-identity" ]; then
        printf '\036mez_shell_identity_begin=%s\n' "$token"
        printf '\036mez_shell_path=/bin/bash\n'
        printf '\036mez_shell_version=GNU bash, version 5.2.26(1)-release\n'
        printf '\036mez_shell_identity_end=%s\n' "$token"
        printf 'mez_bootstrap_field os "Linux"\n'
        printf 'mez_bootstrap_field arch "x86_64"\n'
        printf 'mez_bootstrap_field host "adversarial-host"\n'
        printf 'mez_bootstrap_field user "adversarial-user"\n'
        printf 'mez_bootstrap_field home_directory "/tmp/mez-adversarial-home"\n'
        printf 'mez_bootstrap_field shell_path "/bin/bash"\n'
        printf 'mez_bootstrap_field shell_class "bash"\n'
        printf 'mez_bootstrap_field path "/tmp/mez-adversarial-bin:/usr/bin:/bin"\n'
        printf 'mez_bootstrap_field cwd "/tmp/mez-adversarial-cwd"\n'
        printf 'mez_bootstrap_field git_repo "1"\n'
        printf '\033]133;R;mez_foreign_loader=ready;mez_marker=%s\033\\' "$token"
        printf '\033]133;R;mez_protocol=2;mez_shell=bash;mez_event=child-installed;mez_marker=%s\033\\' "$token"
        printf '\033]133;R;mez_payload_receiver=ready;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\033\\' "$token" "${turn:-spoofed}" "${agent:-spoofed}" "${pane:-%1}"
    fi
    printf '\033]133;D;0;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\033\\' "$token" "${turn:-spoofed}" "${agent:-spoofed}" "${pane:-%1}"
    printf '\n'
done
record "FIXTURE_STDIN_CLOSED"
exit 0
"#;

/// Distinguishing token used to prove the fixture intercepts pane input.
const FIXTURE_INTERCEPT_PROBE: &str = "MEZ-ADVERSARIAL-INTERCEPT-PROBE";
/// Distinguishing token embedded in an agent command body.
const FIXTURE_COMMAND_BODY: &str = "MEZ-ADVERSARIAL-COMMAND-BODY";
/// Marker the fixture records when it observes a control byte.
const FIXTURE_CONTROL_C_MARK: &str = "OBSERVED_CONTROL_C";

/// Writes the adversarial fixture and returns its script and log paths.
fn write_spoof_fixture(root: &Path) -> (PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(root).unwrap();
    let script = root.join("foreground-fixture.sh");
    std::fs::write(&script, SPOOF_FIXTURE_SCRIPT).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, root.join("foreground-fixture.log"))
}

/// Builds a unique scratch directory for one adversarial fixture run.
fn fixture_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "mez-certification-spoof-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    root
}

/// Returns the current fixture log contents, or an empty string before it exists.
fn fixture_log(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Waits until the fixture log contains one observed record.
async fn wait_for_fixture_log(path: &Path, needle: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if fixture_log(path).contains(needle) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "adversarial fixture never observed {needle:?}: {:?}",
            fixture_log(path)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits for a certification snapshot matching one predicate.
async fn wait_for_snapshot<F>(
    handle: &crate::host::async_runtime::AsyncRuntimeSessionHandle,
    pane_id: &str,
    timeout: Duration,
    predicate: F,
) -> crate::host::async_runtime::AsyncPaneCertificationSnapshot
where
    F: Fn(&crate::host::async_runtime::AsyncPaneCertificationSnapshot) -> bool,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = handle.pane_certification_snapshot(pane_id).await.unwrap();
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pane {pane_id} never reached the expected certification state: {snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Runs the production pane supervisor service for one adversarial fixture pane.
async fn spawn_pane_supervisor(
    handle: crate::host::async_runtime::AsyncRuntimeSessionHandle,
    done: StdArc<AtomicBool>,
) {
    let result = run_async_pane_process_supervisor_service(
        handle,
        AsyncPaneProcessSupervisorServiceConfig {
            max_polls: u64::MAX,
            take_limit: 8,
            idle_interval: Duration::from_millis(1),
            pane_service: AsyncPaneProcessServiceConfig {
                max_polls: u64::MAX,
                output_drain_limit: 4,
                drain_limit: 8,
                idle_interval: Duration::from_millis(1),
                foreground_metadata_interval: Duration::from_millis(10),
            },
        },
        move |_, state| {
            done.load(Ordering::SeqCst) || matches!(state, RuntimeLifecycleState::Stopping)
        },
    )
    .await;
    if let Err(error) = result {
        assert!(
            matches!(
                error.message(),
                "async runtime session actor is closed"
                    | "async runtime session actor reply was dropped"
            ),
            "pane supervisor failed before actor shutdown: {error}"
        );
    }
}

/// Returns the first available shell executable from one candidate list.
fn available_shell(candidates: &'static [&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

/// Window in which production must refuse replayed certification frames.
///
/// The runtime's own shell-identity-probe and foreign-bootstrap deadlines are
/// fifteen seconds, so a refusal recorded inside this shorter window proves
/// admission processed the replayed frames instead of letting an untouched pane
/// idle to its bounded deadline.
const SPOOF_REFUSAL_WINDOW: Duration = Duration::from_secs(10);

/// Production-observable refusal recorded for replayed certification frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpoofRefusal {
    /// The runtime retained a stable agent-subshell certification rejection.
    CertificationRejection(&'static str),
    /// The pane's foreign bootstrap settled without shell or environment authority.
    ForeignBootstrapFailed,
}

impl SpoofRefusal {
    /// Returns the stable label used in assertion messages.
    fn as_str(self) -> &'static str {
        match self {
            Self::CertificationRejection(code) => code,
            Self::ForeignBootstrapFailed => "foreign_bootstrap_failed",
        }
    }
}

/// Returns the production refusal published for one pane snapshot, when any.
///
/// Only a pane whose in-band frames were admitted and then refused records one
/// of these states; a pane that never processed the replayed frames records
/// neither a certification rejection nor a settled foreign-bootstrap refusal,
/// so an untouched idle pane yields `None`.
fn production_refusal(
    snapshot: &crate::host::async_runtime::AsyncPaneCertificationSnapshot,
) -> Option<SpoofRefusal> {
    if let Some(rejection) = snapshot.certification_rejection {
        return Some(SpoofRefusal::CertificationRejection(rejection));
    }
    (!snapshot.bootstrap_pending && snapshot.foreign_bootstrap_phase == Some("failed"))
        .then_some(SpoofRefusal::ForeignBootstrapFailed)
}

/// Observed result of one adversarial certification scenario.
struct SpoofCaseOutcome {
    /// Certification snapshot settled by the scenario.
    snapshot: crate::host::async_runtime::AsyncPaneCertificationSnapshot,
    /// Runtime service after the actor shut down.
    service: crate::runtime::RuntimeSessionService,
    /// Fixture log with timeline, screen text, and observed pane input.
    log_text: String,
    /// Foreground process group observed while the fixture owned the pane.
    fixture_process_group: Option<u64>,
    /// Production refusal observed for the replayed frames, when any.
    refusal: Option<SpoofRefusal>,
    /// Whether the fixture observed an interrupt during the run.
    interrupted: bool,
}

/// Runs one adversarial certification scenario end to end.
///
/// The scenario starts the fixture as a real foreground child of the pane's
/// primary shell, proves interception, asks for agent mode, waits for the
/// runtime to settle, then attempts one real agent command while the fixture
/// may still own the pane.
async fn run_foreground_spoof_case(
    shell: &'static str,
    mode: SpoofMode,
    label: &str,
) -> SpoofCaseOutcome {
    let root = fixture_root(label);
    let (script, log) = write_spoof_fixture(&root);
    let mut service = test_service_with_shell(shell);
    service.disable_legacy_managed_startup_for_tests();
    let primary = service
        .attach_primary("primary", true, Size::new(80, 24).unwrap(), 10)
        .unwrap();
    service.start_initial_pane_process(None).unwrap();
    let (handle, actor) = AsyncRuntimeActorFixture::from_service(service)
        .build()
        .unwrap();
    let worker_handle = handle.clone();
    let client_handle = handle.clone();
    let worker_done = StdArc::new(AtomicBool::new(false));
    let worker = spawn_pane_supervisor(worker_handle, StdArc::clone(&worker_done));

    let client = async move {
        wait_for_snapshot(&client_handle, "%1", Duration::from_secs(20), |snapshot| {
            !snapshot.child_active && snapshot.foreground_certified_shell == Some(true)
        })
        .await;

        let launch = format!(
            "sh '{}' '{}' {}\n",
            script.display(),
            log.display(),
            mode.as_str()
        );
        client_handle
            .write_input_to_pane(primary.clone(), "%1", launch.into_bytes())
            .await
            .unwrap();
        let fixture_snapshot =
            wait_for_snapshot(&client_handle, "%1", Duration::from_secs(20), |snapshot| {
                snapshot.foreground_certified_shell == Some(false)
            })
            .await;
        let fixture_process_group =
            fixture_snapshot.foreground_diagnostic["foreground_process_group_id"].as_u64();

        client_handle
            .write_input_to_pane(
                primary.clone(),
                "%1",
                format!("{FIXTURE_INTERCEPT_PROBE}\n").into_bytes(),
            )
            .await
            .unwrap();
        wait_for_fixture_log(
            &log,
            &format!("OBSERVED:{FIXTURE_INTERCEPT_PROBE}"),
            Duration::from_secs(20),
        )
        .await;

        let shown = client_handle
            .execute_terminal_command(primary.clone(), "agent-shell".to_string())
            .await
            .unwrap();
        assert!(shown.contains("agent-shell"), "{shown}");

        let mut timeline = Vec::new();
        let mut settled = None;
        let mut refusal = None;
        // Replayed frames must be refused by production admission instead of
        // being left in an idle pane: require the refusal state the runtime
        // publishes inside a window shorter than its own fifteen-second
        // bootstrap deadlines, so an untouched pane cannot satisfy this loop.
        let refusal_deadline = tokio::time::Instant::now() + SPOOF_REFUSAL_WINDOW;
        for step in 0..1_000 {
            let snapshot = client_handle
                .pane_certification_snapshot("%1")
                .await
                .unwrap();
            if step % 20 == 0 || snapshot.certification_rejection.is_some() {
                timeline.push(format!(
                    "step={step} phase={:?} pending={} rejection={:?} readiness={:?} certified={:?} generation={:?} env={} fg={} primary={}",
                    snapshot.foreign_bootstrap_phase,
                    snapshot.bootstrap_pending,
                    snapshot.certification_rejection,
                    snapshot.readiness,
                    snapshot.foreground_certified_shell,
                    snapshot.shell_interaction_generation,
                    snapshot.environment_signature_present,
                    snapshot.foreground_diagnostic["foreground_process_group_id"],
                    snapshot.foreground_diagnostic["primary_process_id"],
                ));
            }
            if let Some(observed) = production_refusal(&snapshot) {
                refusal = Some(observed);
                settled = Some(snapshot);
                break;
            }
            if snapshot.child_active
                && snapshot.foreground_certified_shell == Some(true)
                && snapshot.foreign_bootstrap_phase == Some("certified")
            {
                settled = Some(snapshot);
                break;
            }
            if tokio::time::Instant::now() >= refusal_deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let snapshot = match settled {
            Some(snapshot) => snapshot,
            None => client_handle
                .pane_certification_snapshot("%1")
                .await
                .unwrap(),
        };

        // Attempt a real agent command while the adversarial program may still
        // own the pane. Its body must never reach a program that never
        // installed an admitted shell receiver.
        let submitted = client_handle
            .execute_agent_shell_command(
                primary.clone(),
                format!("printf '{FIXTURE_COMMAND_BODY}'\n"),
            )
            .await
            .unwrap();
        assert!(
            submitted.contains("running") || submitted.contains("error"),
            "{submitted}"
        );
        tokio::time::sleep(Duration::from_millis(750)).await;

        let screen_text = client_handle
            .managed_shell_process_screen_text("%1")
            .await
            .unwrap_or_default();
        let log_text = format!(
            "timeline:\n{}\nscreen:\n{}\nlog:\n{}\nrefusal:\n{}",
            timeline.join("\n"),
            screen_text,
            fixture_log(&log),
            refusal.map(SpoofRefusal::as_str).unwrap_or("none")
        );
        worker_done.store(true, Ordering::SeqCst);
        let _ = client_handle.shutdown().await.unwrap();
        (snapshot, log_text, refusal, fixture_process_group)
    };

    let ((snapshot, log_text, refusal, fixture_process_group), (), mut actor_exit) =
        tokio::time::timeout(Duration::from_secs(90), async {
            tokio::join!(client, worker, actor.run())
        })
        .await
        .expect("adversarial certification scenario should not hang");
    let interrupted = log_text.contains("SIGNAL_INT");
    actor_exit.service.terminate_all_pane_processes().unwrap();
    SpoofCaseOutcome {
        snapshot,
        service: actor_exit.service,
        log_text,
        fixture_process_group,
        refusal,
        interrupted,
    }
}

/// Asserts the shared acceptance contract for a foreign foreground program.
///
/// `forged_frames` records whether the fixture replayed protocol frames for the
/// marker material it observed, which the silent scenario deliberately does not.
fn assert_no_spoofed_authority(outcome: &SpoofCaseOutcome, forged_frames: bool) {
    let SpoofCaseOutcome {
        snapshot,
        service,
        log_text,
        fixture_process_group,
        refusal,
        interrupted: _,
    } = outcome;
    assert!(
        log_text.contains("OBSERVED:"),
        "the fixture must have observed runtime protocol material: {log_text}"
    );
    if forged_frames {
        assert!(
            log_text.contains("SPOOFED_MARKER:"),
            "the fixture must have replayed forged protocol frames: {log_text}"
        );
        // Proof depth: the contract requires production admission to process
        // the replayed frames and refuse them. The refusal is recorded only
        // inside `SPOOF_REFUSAL_WINDOW`, which is shorter than the runtime's
        // own fifteen-second probe and foreign-bootstrap deadlines, so a run
        // that silently ignored the frames, or merely idled an untouched pane
        // to a deadline, records no refusal and fails here.
        let refusal = refusal.unwrap_or_else(|| {
            panic!(
                "production admission must refuse the replayed certification frames inside \
                 {SPOOF_REFUSAL_WINDOW:?} instead of leaving an untouched idle pane: {log_text}"
            )
        });
        // The refusal must be bound to the replayed frames: the marker the
        // fixture replayed is one production itself delivered on this pane's
        // input, and the replayed start and end frames carried it.
        let replayed_marker = log_text
            .lines()
            .find_map(|line| line.strip_prefix("SPOOFED_MARKER:"))
            .and_then(|rest| rest.split_whitespace().next())
            .expect("the fixture must record the marker it replayed");
        assert!(
            log_text
                .lines()
                .any(|line| line.starts_with("OBSERVED:") && line.contains(replayed_marker)),
            "the replayed marker must be one production delivered on this pane's input: {log_text}"
        );
        assert!(
            !log_text.contains("foreign shell bootstrap timed out")
                && !log_text.contains("managed pane bootstrap timed out"),
            "the replayed frames must be refused by production admission rather than by a \
             bootstrap deadline: {} ({log_text})",
            refusal.as_str()
        );
    }
    let fixture_process_group =
        fixture_process_group.expect("the fixture must be observed as the pane foreground group");
    assert!(
        !matches!(
            service.pane_process_group_is_certified_shell("%1", fixture_process_group as u32),
            Some(true)
        ),
        "a matching foreground process group without an admitted receiver must never be certified: {log_text}"
    );
    assert!(
        snapshot.foreign_bootstrap_phase != Some("certified"),
        "spoofed frames must not settle a certified foreign bootstrap: {snapshot:?}"
    );
    assert!(
        !snapshot.child_active,
        "spoofed frames must not activate an agent subshell: {snapshot:?}"
    );
    assert!(
        !snapshot.environment_signature_present
            && service.pane_environment_signature("%1").is_none(),
        "spoofed bootstrap fields must not publish a pane environment: {snapshot:?}"
    );
    assert!(
        !service.pane_environment_authority_is_certified_for_tests("%1"),
        "spoofed output must not settle certified environment authority"
    );
    // Only a certified environment signature publishes PATH authority, so assert
    // that production state directly instead of collapsing a missing value into
    // an empty string that would pass without observing anything.
    if let Some(signature_path) = service
        .pane_environment_signature("%1")
        .and_then(|signature| signature.path.clone())
    {
        assert!(
            !signature_path.contains("mez-adversarial"),
            "spoofed PATH must never become certified pane environment authority: {signature_path}"
        );
    }
    match service.pane_environment_path("%1") {
        Some(pane_path) => assert!(
            !pane_path.contains("mez-adversarial"),
            "spoofed PATH must never become pane environment authority: {pane_path}"
        ),
        // The accessor falls back to the live pane-process environment, so a
        // missing value is only acceptable while production publishes no
        // signature PATH at all; that published state is asserted above.
        None => assert!(
            service.pane_environment_signature("%1").is_none(),
            "a pane with a published environment signature must expose a readable PATH"
        ),
    }
    let request = mez_agent::shell::PanePathResolutionRequest::new(
        vec![".".to_string()],
        Vec::new(),
        Vec::new(),
    )
    .unwrap();
    // The accessor fails closed while the pane environment is unpublished or
    // unusable. Only those production states are acceptable here: a default of
    // `true` would let an accessor error pass without observing production.
    match service.path_scopes_for_pane_request("%1", &request) {
        Ok(scopes) => assert!(
            scopes.is_none(),
            "spoofed output must not publish pane path authority"
        ),
        Err(error) => {
            let message = error.message();
            assert!(
                message.contains("pane environment is unavailable for path resolution")
                    || message.contains("pane path resolution failed:"),
                "pane path authority must settle unavailable for the spoofed pane: {message}"
            );
        }
    }
    assert!(
        !log_text.contains(FIXTURE_COMMAND_BODY),
        "no agent command body may reach a spoofing foreground program: {log_text}"
    );
    assert_ne!(
        snapshot.readiness,
        mez_agent::PaneReadinessState::Ready,
        "spoofable material must not publish shell-ready authority: {snapshot:?}"
    );
}

/// Proves the adversarial fixture owns the pane foreground group and intercepts
/// runtime input before any contract assertion depends on it.
#[tokio::test(flavor = "current_thread")]
async fn async_adversarial_foreground_fixture_owns_pty_and_intercepts_input() {
    let Some(bash) = available_shell(&["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]) else {
        eprintln!("skipping adversarial foreground fixture probe because Bash is unavailable");
        return;
    };
    let outcome = run_foreground_spoof_case(bash, SpoofMode::Silent, "bash-probe").await;
    assert!(
        outcome.log_text.contains("OBSERVED:"),
        "fixture should have received pane input: {}",
        outcome.log_text
    );
    assert!(
        outcome.fixture_process_group.is_some(),
        "fixture should have owned the pane foreground group: {}",
        outcome.log_text
    );
}

/// Verifies a malicious foreground program that keeps the pane PTY foreground
/// group and replays plausible certification frames cannot publish certified
/// shell identity, environment, or path authority, and cannot obtain a later
/// agent command.
///
/// The fixture observes the runtime's own identity and bootstrap protocol text
/// on its input, derives the marker, turn, agent, and pane metadata from that
/// text, and answers with a plausible prompt plus well-formed OSC start and end
/// frames carrying the observed marker, turn, agent, and pane metadata. That
/// in-band material must not substitute for an admitted receiver that genuinely
/// owns the pane, and it must not leave the pane certified, ready, or holding
/// any environment, path, or command authority.
#[tokio::test(flavor = "current_thread")]
async fn async_spoofed_foreground_program_cannot_publish_shell_authority() {
    let Some(bash) = available_shell(&["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]) else {
        eprintln!("skipping adversarial Bash certification test because Bash is unavailable");
        return;
    };
    let outcome = run_foreground_spoof_case(bash, SpoofMode::Spoof, "bash-authority").await;
    assert_no_spoofed_authority(&outcome, true);
}

/// Verifies a foreground program that never answers certification keeps the
/// pane, is never killed or interrupted, and never receives an agent command.
///
/// The runtime may keep probing inside its bounded observation budget, but it
/// must not force settlement by signalling the program that owns the PTY.
#[tokio::test(flavor = "current_thread")]
async fn async_silent_foreground_program_keeps_pane_and_receives_no_command() {
    let Some(bash) = available_shell(&["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]) else {
        eprintln!("skipping adversarial silent certification test because Bash is unavailable");
        return;
    };
    let outcome = run_foreground_spoof_case(bash, SpoofMode::Silent, "bash-silent").await;
    assert_no_spoofed_authority(&outcome, false);
    assert!(
        !outcome.interrupted,
        "an unresponsive foreground program must not be interrupted by certification: {}",
        outcome.log_text
    );
    assert!(
        !outcome.log_text.contains(FIXTURE_CONTROL_C_MARK),
        "no implicit Ctrl-C may reach an unresponsive foreground program: {}",
        outcome.log_text
    );
    assert_eq!(
        outcome.snapshot.foreground_certified_shell,
        Some(false),
        "the pane must remain owned by the unresponsive program: {}",
        outcome.log_text
    );
}

/// Verifies the same spoofing contract when the pane's primary shell is Fish.
#[tokio::test(flavor = "current_thread")]
async fn async_spoofed_foreground_program_cannot_publish_authority_under_fish() {
    let Some(fish) = available_shell(&[
        "/usr/bin/fish",
        "/usr/local/bin/fish",
        "/opt/homebrew/bin/fish",
    ]) else {
        eprintln!("skipping adversarial Fish certification test because fish is unavailable");
        return;
    };
    let outcome = run_foreground_spoof_case(fish, SpoofMode::Spoof, "fish-authority").await;
    assert_no_spoofed_authority(&outcome, true);
}

/// Verifies the same spoofing contract when the pane's primary shell is Zsh.
#[tokio::test(flavor = "current_thread")]
async fn async_spoofed_foreground_program_cannot_publish_authority_under_zsh() {
    let Some(zsh) = available_shell(&["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"]) else {
        eprintln!("skipping adversarial Zsh certification test because zsh is unavailable");
        return;
    };
    let outcome = run_foreground_spoof_case(zsh, SpoofMode::Spoof, "zsh-authority").await;
    assert_no_spoofed_authority(&outcome, true);
}

/// Verifies replayed in-band identity and bootstrap material cannot abort the
/// runtime and settles to a terminal bounded bootstrap phase.
///
/// Minimal reproduction: a foreground child of the pane's primary shell keeps
/// the pane PTY foreground process group, observes the runtime's own identity
/// probe text, and answers with well-formed OSC start/end frames that carry the
/// observed marker, turn, agent, and pane metadata plus forged in-band
/// `mez_shell_identity_*` records, bootstrap environment fields, and loader or
/// receiver frames. Nothing in that material is secret: the runtime delivers
/// the probe text to whatever owns the PTY, so any program that reads its own
/// stdin can replay it. That input previously performed the dependency-free
/// foreign child handoff on the deep pane-output observation stack and aborted
/// the whole process with a stack overflow (SIGABRT).
///
/// The identity settlement now records an explicit `child-launch-pending`
/// boundary phase and the reconciliation stack dispatches the child launch and
/// receiver-completed transactions, so the replayed forgery settles in bounded
/// time instead of exhausting the output-observation stack. The admission
/// provenance of in-band identity evidence is tracked separately; this
/// regression owns the process-abort failure and the bounded settlement.
#[tokio::test(flavor = "current_thread")]
async fn async_forged_identity_records_settle_without_abort() {
    let Some(bash) = available_shell(&["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]) else {
        eprintln!("skipping forged identity regression because Bash is unavailable");
        return;
    };
    let outcome = run_foreground_spoof_case(bash, SpoofMode::ForgedIdentity, "bash-forged").await;
    assert!(
        outcome.log_text.contains("SPOOFED_MARKER:"),
        "the fixture must have replayed forged identity and bootstrap frames: {}",
        outcome.log_text
    );
    // Reaching this assertion proves the runtime did not abort the process; the
    // scenario harness fails closed on a hang before its own bound instead.
    assert!(
        matches!(
            outcome.snapshot.foreign_bootstrap_phase,
            Some("certified") | Some("failed")
        ) && !outcome.snapshot.bootstrap_pending,
        "replayed forgery must settle to a terminal bounded bootstrap phase: {:?}",
        outcome.snapshot
    );
    eprintln!(
        "forged identity scenario settled without aborting the runtime: {:?}",
        outcome.snapshot
    );
}

/// Verifies the replayed-forgery path settles once without re-entering the
/// foreign identity and bootstrap transition.
///
/// The settled service state must be terminal for the pane: no running shell
/// transaction (including a re-registered identity probe or bootstrap) and no
/// pending bootstrap, so the replay cannot leave the runtime mid-transition
/// where a later event re-enters the handoff.
#[tokio::test(flavor = "current_thread")]
async fn async_replayed_forgery_settles_without_bootstrap_reentry() {
    let Some(bash) = available_shell(&["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"]) else {
        eprintln!("skipping forged identity settlement regression because Bash is unavailable");
        return;
    };
    let outcome =
        run_foreground_spoof_case(bash, SpoofMode::ForgedIdentity, "bash-forged-settle").await;
    assert!(
        !outcome.service.pane_bootstrap_is_pending_for_tests("%1"),
        "replayed forgery must not leave a pending bootstrap: {}",
        outcome.log_text
    );
    assert!(
        !outcome
            .service
            .running_shell_transactions_for_tests()
            .values()
            .any(|transaction| transaction.pane_id == "%1"),
        "replayed forgery must settle without a running shell transaction: {}",
        outcome.log_text
    );
    assert!(
        matches!(
            outcome
                .service
                .foreign_shell_bootstrap_phase_for_tests("%1"),
            Some("certified") | Some("failed")
        ),
        "the settled foreign bootstrap phase must be terminal: {}",
        outcome.log_text
    );
}
