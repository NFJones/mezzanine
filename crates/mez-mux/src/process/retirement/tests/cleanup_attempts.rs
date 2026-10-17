//! Cleanup attempt admission, source ABA and ordinary sibling lifecycle tests.
//!
//! ControlledChild cases classify ownership/rejection without claiming OS
//! signals or reaping. Real PTY cases retain duplicate masters, require fresh
//! child-generated responses, and use actual waits plus strict original SRCH.

use super::*;
use std::os::fd::OwnedFd;

/// Snapshots every private bookkeeping/progress field without polling a child.
fn snapshot(owner: &PaneProcessRetirementOwner) -> String {
    let state = owner.state();
    format!("{state:?}; capacity={}", state.slots.capacity())
}

/// Returns an opaque controlled obligation, never a numeric signal target.
fn returned(owner: &PaneProcessRetirementOwner) -> Arc<ChildState> {
    let state = Arc::new(ChildState::default());
    drop(
        owner
            .reserve()
            .unwrap()
            .attach(Box::new(ControlledChild(state.clone())), None),
    );
    state
}

/// Releases controlled wait gates and observes their exact cached waits only.
fn settle_controlled(owner: &PaneProcessRetirementOwner, states: &[Arc<ChildState>]) {
    for state in states {
        state.exited.store(true, Ordering::SeqCst);
    }
    let mut drain = owner.begin_drain().unwrap();
    while let Some(mut child) = drain.take_returned() {
        assert!(child.try_wait().unwrap().is_some());
    }
    assert_eq!(owner.outstanding(), 0);
    for state in states {
        assert_eq!(state.kills.load(Ordering::SeqCst), 0);
    }
}

/// Reserved, Leased, Draining and active-empty-claim serialization all reject
/// explicit admission before any field changes, killer call or slot movement.
#[test]
fn explicit_attempt_busy_matrix_is_mutation_free() {
    let owner = PaneProcessRetirementOwner::default();
    let reservation = owner.reserve().unwrap();
    owner.seal();
    let prepared = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let before = snapshot(&owner);
    assert!(prepared.admit().is_err());
    assert_eq!(snapshot(&owner), before);
    drop(reservation);

    let owner = PaneProcessRetirementOwner::default();
    let state = Arc::new(ChildState::default());
    let mut child = owner
        .reserve()
        .unwrap()
        .attach(Box::new(ControlledChild(state.clone())), None);
    owner.seal();
    let prepared = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let before = snapshot(&owner);
    assert!(prepared.admit().is_err());
    assert_eq!(snapshot(&owner), before);
    state.exited.store(true, Ordering::SeqCst);
    assert!(child.try_wait().unwrap().is_some());
    assert_eq!(state.kills.load(Ordering::SeqCst), 0);

    for claim in [false, true] {
        let owner = PaneProcessRetirementOwner::default();
        let state = returned(&owner);
        owner.seal();
        let token = owner
            .capture_cleanup_attempt(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let mut drain = owner.try_begin_drain_for_attempt(&token).unwrap().unwrap();
        let child = claim.then(|| drain.take_returned().unwrap());
        let prepared = owner
            .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let before = snapshot(&owner);
        assert!(prepared.admit().is_err());
        assert_eq!(snapshot(&owner), before);
        drop(child);
        drop(drain);
        settle_controlled(&owner, &[state]);
    }
}

/// None-to-initial0 registration, foreign tokens, expired preparations and
/// both bounded generation overflows reject without rewriting authority.
#[test]
fn stale_foreign_deadline_and_overflow_rejections_preserve_state() {
    let owner = PaneProcessRetirementOwner::default();
    let state = returned(&owner);
    owner.seal();
    let old = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let token = owner
        .capture_cleanup_attempt(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let before = snapshot(&owner);
    assert!(old.admit().is_err());
    assert_eq!(snapshot(&owner), before);
    assert!(
        owner
            .prepare_explicit_cleanup_retry(Instant::now() - Duration::from_millis(1))
            .is_err()
    );
    assert_eq!(snapshot(&owner), before);
    let delayed = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_millis(2))
        .unwrap();
    std::thread::sleep(Duration::from_millis(5));
    assert!(delayed.admit().is_err());
    assert_eq!(snapshot(&owner), before);

    let foreign = PaneProcessRetirementOwner::default();
    let foreign_state = returned(&foreign);
    let foreign_before = snapshot(&foreign);
    assert!(foreign.try_begin_drain_for_attempt(&token).is_err());
    assert_eq!(snapshot(&foreign), foreign_before);
    settle_controlled(&foreign, &[foreign_state]);

    owner.state().attempt = Some(CleanupAttemptState {
        epoch: u64::MAX,
        deadline: Some(Instant::now() - Duration::from_millis(1)),
        force: true,
    });
    let prepared = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let before = snapshot(&owner);
    assert!(prepared.admit().is_err());
    assert_eq!(snapshot(&owner), before);
    settle_controlled(&owner, &[state]);

    let source = PaneProcessRetirementOwner::default();
    source.state().source_epoch = u64::MAX;
    let before = snapshot(&source);
    assert!(source.reserve().is_err());
    assert_eq!(snapshot(&source), before);
}

/// Explicit epochs preserve immutable raw origin, already-sent stages,
/// successful-force progress and the first failure through native zero. This
/// is classified synthetic progress, not a claim of a real root signal.
#[test]
fn new_attempt_preserves_origin_stages_force_and_first_failure() {
    let owner = PaneProcessRetirementOwner::default();
    let state = Arc::new(ChildState::default());
    let mut child = owner
        .reserve()
        .unwrap()
        .attach(Box::new(ControlledChild(state.clone())), None);
    child.begin_termination(false).unwrap();
    let raw = child.raw.as_mut().unwrap();
    raw.progress.plan.as_mut().unwrap().deadline = Instant::now() - Duration::from_millis(1);
    raw.progress.hup_sent = true;
    raw.progress.term_sent = true;
    raw.force_requested = true;
    let origin = raw.progress.plan.unwrap();
    child.record_failure(
        "classified first stage",
        &MezError::io("sticky native fixture cause"),
    );
    let failure = owner.cleanup_failure().unwrap();
    drop(child);
    owner.seal();
    let old = owner
        .capture_cleanup_attempt(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let token = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap()
        .admit()
        .unwrap();
    assert!(old.validate(&owner).is_err());
    let before = snapshot(&owner);
    assert!(owner.try_begin_drain_for_attempt(&old).is_err());
    assert_eq!(snapshot(&owner), before);
    let mut drain = owner.try_begin_drain_for_attempt(&token).unwrap().unwrap();
    let mut child = drain.take_returned().unwrap();
    assert!(
        child.force_retirement().is_ok(),
        "successful force must not be replayed"
    );
    let raw = child.raw.as_ref().unwrap();
    assert_eq!(raw.progress.plan.unwrap().started, origin.started);
    assert_eq!(raw.progress.plan.unwrap().deadline, origin.deadline);
    assert!(raw.progress.hup_sent && raw.progress.term_sent && raw.force_requested);
    assert_eq!(state.kills.load(Ordering::SeqCst), 0);
    state.exited.store(true, Ordering::SeqCst);
    assert!(child.try_wait().unwrap().is_some());
    assert!(child.force_retirement().is_ok());
    assert_eq!(owner.outstanding(), 0);
    assert_eq!(owner.cleanup_failure().as_deref(), Some(failure.as_str()));
}

/// A new automatic/initial0 budget cannot renew an expired raw plan. It may
/// observe a later native wait, but cannot invoke the unknown portable killer.
#[test]
fn initial_continuation_does_not_renew_raw_deadline() {
    let owner = PaneProcessRetirementOwner::default();
    let state = Arc::new(ChildState::default());
    let mut child = owner
        .reserve()
        .unwrap()
        .attach(Box::new(ControlledChild(state.clone())), None);
    child.begin_termination(false).unwrap();
    child
        .raw
        .as_mut()
        .unwrap()
        .progress
        .plan
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    drop(child);
    let token = owner
        .capture_cleanup_attempt(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let mut drain = owner.try_begin_drain_for_attempt(&token).unwrap().unwrap();
    let mut child = drain.take_returned().unwrap();
    assert!(
        child
            .poll_cleanup()
            .unwrap_err()
            .message()
            .contains("deadline expired")
    );
    assert_eq!(state.kills.load(Ordering::SeqCst), 0);
    state.exited.store(true, Ordering::SeqCst);
    assert!(child.try_wait().unwrap().is_some());
}

/// Cancelling an admitted claim returns its scalar binding and cannot mint or
/// renew. A live explicit call continues the same fixed budget; only a later
/// genuinely expired explicit admission advances again. Force progress here is
/// classified synthetic state, not OS death evidence.
#[test]
fn cancelled_retry_claim_keeps_budget_until_distinct_expired_admission() {
    let owner = PaneProcessRetirementOwner::default();
    let state = Arc::new(ChildState::default());
    let mut child = owner
        .reserve()
        .unwrap()
        .attach(Box::new(ControlledChild(state.clone())), None);
    child.begin_termination(true).unwrap();
    child
        .raw
        .as_mut()
        .unwrap()
        .progress
        .plan
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    child.raw.as_mut().unwrap().force_requested = true;
    drop(child);
    owner.seal();
    let token = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_millis(200))
        .unwrap()
        .admit()
        .unwrap();
    let original_deadline = token.deadline();
    let mut drain = owner.try_begin_drain_for_attempt(&token).unwrap().unwrap();
    let mut child = drain.take_returned().unwrap();
    assert!(child.poll_cleanup().unwrap().is_none());
    drop(child);
    drop(drain);
    let continuation = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap()
        .admit()
        .unwrap();
    assert_eq!(continuation.binding.epoch, token.binding.epoch);
    assert_eq!(continuation.deadline(), original_deadline);
    std::thread::sleep(Duration::from_millis(210));
    let next = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap()
        .admit()
        .unwrap();
    assert_eq!(next.binding.epoch, token.binding.epoch + 1);
    assert!(token.validate(&owner).is_err());
    assert_eq!(state.kills.load(Ordering::SeqCst), 0);
    settle_controlled(&owner, &[state]);
}

/// Sparse vacant slots survive initial admission unchanged and capacity is
/// reclaimed only after exact waits, not a synthetic absolute live-child cap.
#[test]
fn initial_sparse_and_zero_resource_admission_do_not_mint_retry() {
    let owner = PaneProcessRetirementOwner::default();
    let states = (0..3)
        .map(|_| Arc::new(ChildState::default()))
        .collect::<Vec<_>>();
    let mut children = states
        .iter()
        .map(|state| {
            owner
                .reserve()
                .unwrap()
                .attach(Box::new(ControlledChild(state.clone())), None)
        })
        .collect::<Vec<_>>();
    states[1].exited.store(true, Ordering::SeqCst);
    assert!(children[1].try_wait().unwrap().is_some());
    drop(children);
    owner.seal();
    let token = owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap()
        .admit()
        .unwrap();
    assert_eq!(token.binding.epoch, 0);
    assert!(owner.state().slots[1].is_none());
    settle_controlled(&owner, &states);
    assert_eq!(owner.state().slots.capacity(), 0);
    let before = snapshot(&owner);
    owner
        .prepare_explicit_cleanup_retry(Instant::now() + Duration::from_secs(1))
        .unwrap()
        .admit()
        .unwrap();
    assert_eq!(snapshot(&owner), before);
}

/// Writes only a bounded test-owned PTY protocol, never a numeric signal.
fn input(master: &OwnedFd, mut bytes: &[u8]) -> bool {
    let deadline = Instant::now() + Duration::from_millis(500);
    while !bytes.is_empty() && Instant::now() < deadline {
        match rustix::io::write(master, bytes) {
            Ok(0) => return false,
            Ok(count) => bytes = &bytes[count..],
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(_) => return false,
        }
        if !bytes.is_empty() {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    bytes.is_empty()
}

/// Reads a child-generated response through a held nonblocking master.
fn marker(master: &OwnedFd, expected: &str) -> bool {
    let deadline = Instant::now() + Duration::from_millis(500);
    let mut output = Vec::new();
    while Instant::now() < deadline && output.len() < 64 * 1024 {
        let mut bytes = [0u8; 4096];
        match rustix::io::read(master, &mut bytes) {
            Ok(0) => break,
            Ok(count) => output.extend_from_slice(&bytes[..count]),
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(_) => break,
        }
        if String::from_utf8_lossy(&output).contains(expected) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

/// Starts one real root with echo disabled and an independent finish protocol.
fn reply_process(
    owner: &PaneProcessRetirementOwner,
) -> (
    super::super::super::PaneProcess,
    OwnedFd,
    rustix::process::Pid,
) {
    let process = super::super::super::spawn_retained_argv_pty_process(std::path::Path::new("/bin/sh"),
        &["-c".into(), "trap '' HUP TERM; stty -echo; printf 'attempt-ready\\n'; while IFS= read -r line; do if [ \"$line\" = fixture-finish ]; then exit 0; fi; printf 'attempt-child:%s\\n' \"$line\"; done".into()],
        &[], mez_terminal::TerminalSize::new(80, 24).unwrap(), None, owner.reserve().unwrap()).unwrap();
    let pid =
        rustix::process::Pid::from_raw(i32::try_from(process.primary_pid()).unwrap()).unwrap();
    let master = process.duplicate_master_fd().unwrap();
    assert!(marker(&master, "attempt-ready"));
    (process, master, pid)
}

/// Polls the exact native process until its real wait succeeds, within one
/// fixture budget. No process disappearance probe substitutes for that wait.
fn wait_process(process: &mut super::super::super::PaneProcess) -> bool {
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if matches!(process.poll_exit(), Ok(Some(_))) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

/// Pane A's expired local plan must not poison healthy Leased B or newly
/// admitted C in the same nonempty owner. Require fresh B/C replies before
/// independently finishing and actually waiting/reaping those roots.
#[test]
fn local_expiry_does_not_globalize_across_live_sibling_and_new_child() {
    let owner = PaneProcessRetirementOwner::default();
    let (mut a, master_a, pid_a) = reply_process(&owner);
    let (mut b, master_b, pid_b) = reply_process(&owner);
    a.begin_native_termination(false).unwrap();
    a.child
        .raw
        .as_mut()
        .unwrap()
        .progress
        .plan
        .as_mut()
        .unwrap()
        .deadline = Instant::now() - Duration::from_millis(1);
    let expired = a.poll_native_termination(true);
    let b_unplanned = b.child.raw.as_ref().unwrap().progress.plan.is_none();
    let no_shared_budget = {
        let state = owner.state();
        state.shutdown.is_none() && state.attempt.unwrap().deadline.is_none()
    };
    let finished_a = input(&master_a, b"fixture-finish\n") && wait_process(&mut a);
    let absent_a = rustix::process::test_kill_process(pid_a) == Err(rustix::io::Errno::SRCH);
    let (mut c, master_c, pid_c) = reply_process(&owner);
    let b_response =
        input(&master_b, b"live-b-probe\n") && marker(&master_b, "attempt-child:live-b-probe");
    let c_response =
        input(&master_c, b"fresh-c-probe\n") && marker(&master_c, "attempt-child:fresh-c-probe");
    let finished_b = input(&master_b, b"fixture-finish\n") && wait_process(&mut b);
    let finished_c = input(&master_c, b"fixture-finish\n") && wait_process(&mut c);
    let all_absent = [pid_a, pid_b, pid_c]
        .into_iter()
        .all(|pid| rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH));
    drop((master_a, master_b, master_c));
    assert!(
        finished_a && finished_b && finished_c && all_absent && owner.outstanding() == 0,
        "owned fixture waits did not settle"
    );
    assert!(expired.is_err() && absent_a && b_unplanned && no_shared_budget);
    assert!(
        b_response && c_response,
        "local expiry affected a healthy or fresh sibling"
    );
}

/// Idle source reuse with the old drain still retained cannot let that drain
/// claim a new root or overwrite the new cursor. Prove the new root responds
/// before independent protocol finish and exact native wait/SRCH.
#[test]
fn idle_source_reuse_fences_old_drain_scope_and_cached_child() {
    let owner = PaneProcessRetirementOwner::default();
    let (a, master_a, pid_a) = reply_process(&owner);
    drop(a);
    let old = owner
        .capture_cleanup_attempt(Instant::now() + Duration::from_secs(1))
        .unwrap();
    let mut drain = owner.try_begin_drain_for_attempt(&old).unwrap().unwrap();
    let mut child_a = drain.take_returned().unwrap();
    child_a.force_retirement().unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while child_a.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        rustix::process::test_kill_process(pid_a),
        Err(rustix::io::Errno::SRCH)
    );
    let (b, master_b, pid_b) = reply_process(&owner);
    drop(b);
    let wrongly_claimed = drain.take_returned();
    let old_claimed_fresh = wrongly_claimed.is_some();
    let old_stale = old.validate(&owner).is_err();
    child_a.force_retirement().unwrap();
    let b_response = input(&master_b, b"new-source-probe\n")
        && marker(&master_b, "attempt-child:new-source-probe");
    drain.cursor = 999;
    drop(drain);
    let cursor_preserved = owner.state().drain_cursor == 0;
    let finished = input(&master_b, b"fixture-finish\n");
    let mut fresh_drain = owner.begin_drain().unwrap();
    let mut child_b = wrongly_claimed
        .or_else(|| fresh_drain.take_returned())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut waited = false;
    while Instant::now() < deadline {
        if child_b.try_wait().unwrap().is_some() {
            waited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let absent = rustix::process::test_kill_process(pid_b) == Err(rustix::io::Errno::SRCH);
    drop((master_a, master_b));
    assert!(
        finished && waited && absent && owner.outstanding() == 0,
        "fresh fixture did not settle naturally"
    );
    assert!(
        old_stale && !old_claimed_fresh && b_response && cursor_preserved,
        "old source touched fresh native ownership"
    );
}
