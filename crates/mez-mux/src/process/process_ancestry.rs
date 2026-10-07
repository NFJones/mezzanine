//! Bounded native process-parent evidence for local observational producers.
//!
//! Each observation pairs PID, parent PID and kernel start time from one native
//! record. An ancestry walk accepts exact endpoint incarnations, limits both
//! depth and elapsed time, and reobserves every link before returning. It must
//! run off the serialized actor; callers still fence the adapter-owned root
//! generation and producer at commit. Two passes are not an atomic process-tree
//! snapshot, nor proof of executable/vendor identity, current socket writer,
//! credentials, or client/session association. No hints or subprocess fallbacks
//! replace unavailable native evidence.

use std::time::{Duration, Instant};

/// Maximum number of live observations retained by an ancestry walk.
const MAX_ANCESTRY_DEPTH: usize = 128;
/// Cooperative walk budget; a single native syscall cannot be hard-cancelled.
const ANCESTRY_BUDGET: Duration = Duration::from_millis(100);

/// Parent and incarnation obtained together from one native process record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessParentIdentity {
    /// Positive PID named by the native record.
    pub process_id: u32,
    /// Parent PID at observation time; zero denotes no observable parent.
    pub parent_process_id: u32,
    /// Opaque kernel start token, using the existing pane identity convention.
    pub start_token: u64,
}

/// Exact observed path from connection origin through its expected pane root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessAncestry {
    /// Child-first chain including both endpoints, with at most 128 entries.
    pub chain: Vec<ProcessParentIdentity>,
}

/// Fail-closed reason a bounded process-tree observation cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessAncestryUnavailable {
    /// Native metadata is unsupported, unreadable, malformed, or no longer live.
    Unreadable,
    /// An endpoint incarnation or an observed parent link changed.
    Changed,
    /// The expected root is not an ancestor, or the native tree is inconsistent.
    Unrelated,
    /// Depth or cooperative elapsed-time bound was exhausted.
    LimitExceeded,
}

/// Reads one bounded Linux stat record; no executable, argv or environment is
/// read or retained. Procfs errors, non-UTF-8 records and overlong data fail closed.
#[cfg(target_os = "linux")]
pub fn process_parent_identity_for_pid(pid: u32) -> Option<ProcessParentIdentity> {
    use std::io::Read;

    if pid == 0 {
        return None;
    }
    let file = std::fs::File::open(format!("/proc/{pid}/stat")).ok()?;
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 8192 {
        return None;
    }
    parse_linux_parent_identity(pid, std::str::from_utf8(&bytes).ok()?)
}

/// Parses PID, parent and start time without splitting the link-bearing comm
/// field. Rejects dead/unknown states, wrong record PIDs and incomplete records.
#[cfg(any(target_os = "linux", test))]
fn parse_linux_parent_identity(pid: u32, stat: &str) -> Option<ProcessParentIdentity> {
    let command_start = stat.find('(')?;
    let record_pid = stat.get(..command_start)?.trim().parse::<u32>().ok()?;
    if pid == 0 || record_pid != pid {
        return None;
    }
    let command_end = stat.rfind(')')?;
    if command_end < command_start {
        return None;
    }
    let mut fields = stat.get(command_end + 1..)?.split_whitespace();
    if !matches!(
        fields.next()?,
        "R" | "S" | "D" | "T" | "t" | "I" | "W" | "P"
    ) {
        return None;
    }
    let parent_process_id = fields.next()?.parse().ok()?;
    // State and parent are fields 3 and 4; starttime is field 22.
    let start_token = fields.nth(17)?.parse().ok()?;
    Some(ProcessParentIdentity {
        process_id: pid,
        parent_process_id,
        start_token,
    })
}

/// Reads parent and start time from an exact Darwin PROC_PIDTBSDINFO result.
/// Unsupported/denied metadata and zombies never become guessed ancestry.
#[cfg(target_os = "macos")]
pub fn process_parent_identity_for_pid(pid: u32) -> Option<ProcessParentIdentity> {
    use std::mem::{MaybeUninit, size_of};

    let native_pid = libc::c_int::try_from(pid).ok().filter(|pid| *pid > 0)?;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = libc::c_int::try_from(size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: the initialized output has the exact native size and remains live
    // through proc_pidinfo. It is consumed only after an exact-size result.
    let actual = unsafe {
        libc::proc_pidinfo(
            native_pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if actual != size {
        return None;
    }
    // SAFETY: the full initialized native record was returned above.
    let info = unsafe { info.assume_init() };
    if info.pbi_pid != pid || info.pbi_status == libc::SZOMB {
        return None;
    }
    Some(ProcessParentIdentity {
        process_id: pid,
        parent_process_id: info.pbi_ppid,
        start_token: info
            .pbi_start_tvsec
            .checked_mul(1_000_000)?
            .checked_add(info.pbi_start_tvusec)?,
    })
}

/// Fails closed on platforms without a reviewed native parent/start reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_parent_identity_for_pid(_pid: u32) -> Option<ProcessParentIdentity> {
    None
}

/// Resolves exact endpoint observations and rechecks the full descendant chain.
/// Call on a bounded worker, never on the serialized actor. Retained evidence
/// requires fresh actor-side root generation/producer checks before use.
pub fn process_ancestry(
    origin: ProcessParentIdentity,
    root: ProcessParentIdentity,
) -> Result<ProcessAncestry, ProcessAncestryUnavailable> {
    let started = Instant::now();
    resolve_ancestry(origin, root, process_parent_identity_for_pid, || {
        started.elapsed() >= ANCESTRY_BUDGET
    })
}

/// Walks and reobserves native links using injected readers for deterministic
/// replacement, reparenting and budget regressions without live PID races.
fn resolve_ancestry(
    origin: ProcessParentIdentity,
    root: ProcessParentIdentity,
    mut read: impl FnMut(u32) -> Option<ProcessParentIdentity>,
    mut expired: impl FnMut() -> bool,
) -> Result<ProcessAncestry, ProcessAncestryUnavailable> {
    use ProcessAncestryUnavailable as Failure;

    if origin.process_id == 0 || root.process_id == 0 {
        return Err(Failure::Unreadable);
    }
    let mut chain: Vec<ProcessParentIdentity> = Vec::new();
    let mut pid = origin.process_id;
    loop {
        if expired() || chain.len() >= MAX_ANCESTRY_DEPTH {
            return Err(Failure::LimitExceeded);
        }
        if pid == 0 || chain.iter().any(|entry| entry.process_id == pid) {
            return Err(Failure::Unrelated);
        }
        let observed = read(pid).ok_or(Failure::Unreadable)?;
        if expired() {
            return Err(Failure::LimitExceeded);
        }
        if observed.process_id != pid {
            return Err(Failure::Changed);
        }
        if (pid == origin.process_id && observed != origin)
            || (pid == root.process_id && observed != root)
        {
            return Err(Failure::Changed);
        }
        if chain
            .last()
            .is_some_and(|child| observed.start_token > child.start_token)
        {
            return Err(Failure::Unrelated);
        }
        chain.push(observed);
        if pid == root.process_id {
            break;
        }
        pid = observed.parent_process_id;
    }
    // Reobserve root first and origin last. This catches persistent link changes,
    // not an atomic tree snapshot or an ABA reparenting between observations.
    for expected in chain.iter().rev() {
        if expired() {
            return Err(Failure::LimitExceeded);
        }
        let current = read(expected.process_id).ok_or(Failure::Unreadable)?;
        if expired() {
            return Err(Failure::LimitExceeded);
        }
        if current != *expected {
            return Err(Failure::Changed);
        }
    }
    Ok(ProcessAncestry { chain })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates inert native-record fixtures with explicit PID and incarnation.
    fn node(pid: u32, parent: u32) -> ProcessParentIdentity {
        ProcessParentIdentity {
            process_id: pid,
            parent_process_id: parent,
            start_token: u64::from(pid),
        }
    }

    /// Native comm may contain spaces, newlines and parentheses. Parent/start
    /// fields must remain paired and wrong PIDs, dead or malformed states fail.
    #[test]
    fn ancestry_linux_stat_parser_pairs_parent_and_start() {
        let stat = "7314 (weird (name)\n with spaces) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 424242 20";
        assert_eq!(
            parse_linux_parent_identity(7314, stat),
            Some(ProcessParentIdentity {
                process_id: 7314,
                parent_process_id: 1,
                start_token: 424242
            })
        );
        assert_eq!(parse_linux_parent_identity(9, stat), None);
        for state in ["Z", "X", "x", "?", "RR"] {
            assert_eq!(
                parse_linux_parent_identity(7314, &stat.replace(") R ", &format!(") {state} "))),
                None
            );
        }
        for stat in ["", "7314 (x) S 1", "7314 (x S 1", "0 (x) R 1"] {
            assert_eq!(parse_linux_parent_identity(7314, stat), None);
        }
    }

    /// Exact ancestry includes a helper and stable producer without choosing
    /// either as a vendor owner; same-root observation is also valid evidence.
    #[test]
    fn ancestry_resolves_and_reobserves_every_link() {
        let nodes = [node(30, 20), node(20, 10), node(10, 1)];
        let mut reads = Vec::new();
        let result = resolve_ancestry(
            nodes[0],
            nodes[2],
            |pid| {
                reads.push(pid);
                nodes.iter().find(|n| n.process_id == pid).copied()
            },
            || false,
        )
        .unwrap();
        assert_eq!(result.chain, nodes);
        assert_eq!(reads, [30, 20, 10, 10, 20, 30]);
        assert_eq!(
            resolve_ancestry(nodes[2], nodes[2], |_| Some(nodes[2]), || false)
                .unwrap()
                .chain,
            [nodes[2]]
        );
    }

    /// Every endpoint and intermediate parent/start field is fenced; PID reuse
    /// and helper reparenting cannot silently preserve a previously observed path.
    #[test]
    fn ancestry_rejects_replacement_and_reparenting() {
        let nodes = [node(30, 20), node(20, 10), node(10, 1)];
        for changed_pid in [10, 20, 30] {
            for change_parent in [false, true] {
                let mut reads = 0;
                let result = resolve_ancestry(
                    nodes[0],
                    nodes[2],
                    |pid| {
                        reads += 1;
                        let mut n = *nodes.iter().find(|n| n.process_id == pid)?;
                        if reads > 3 && pid == changed_pid {
                            if change_parent {
                                n.parent_process_id = 7;
                            } else {
                                n.start_token += 1;
                            }
                        }
                        Some(n)
                    },
                    || false,
                );
                assert_eq!(result, Err(ProcessAncestryUnavailable::Changed));
            }
        }
        let mut stale = nodes[0];
        stale.start_token += 1;
        assert_eq!(
            resolve_ancestry(
                stale,
                nodes[2],
                |pid| nodes.iter().find(|n| n.process_id == pid).copied(),
                || false
            ),
            Err(ProcessAncestryUnavailable::Changed)
        );
    }

    /// Missing metadata, unrelated/cyclic trees and a younger parent all fail
    /// closed; no PID/environment fallback or successful partial chain exists.
    #[test]
    fn ancestry_rejects_unreadable_unrelated_and_inconsistent_trees() {
        let origin = node(30, 20);
        let root = node(10, 1);
        assert_eq!(
            resolve_ancestry(origin, root, |_| None, || false),
            Err(ProcessAncestryUnavailable::Unreadable)
        );
        for intermediate in [
            node(20, 0),
            node(20, 30),
            ProcessParentIdentity {
                start_token: 31,
                ..node(20, 10)
            },
        ] {
            assert_eq!(
                resolve_ancestry(
                    origin,
                    root,
                    |pid| if pid == 30 {
                        Some(origin)
                    } else {
                        Some(intermediate)
                    },
                    || false
                ),
                Err(ProcessAncestryUnavailable::Unrelated)
            );
        }
    }

    /// Depth and elapsed budgets are enforced during collection and recheck;
    /// admission cannot request unbounded scans through a supplied process tree.
    #[test]
    fn ancestry_limits_collection_and_reobservation() {
        assert_eq!(
            resolve_ancestry(
                node(200, 199),
                node(1, 0),
                |pid| Some(node(pid, pid - 1)),
                || false
            ),
            Err(ProcessAncestryUnavailable::LimitExceeded)
        );
        let mut ticks = 0;
        assert_eq!(
            resolve_ancestry(
                node(2, 1),
                node(1, 0),
                |pid| Some(node(pid, pid - 1)),
                || {
                    ticks += 1;
                    ticks > 2
                }
            ),
            Err(ProcessAncestryUnavailable::LimitExceeded)
        );
    }

    /// A native read can cross the budget while it is in flight. Even the last
    /// reobservation must reject the returned evidence after expiry, without
    /// pretending the synchronous read was cancelled or accepting a late result.
    #[test]
    fn ancestry_rejects_budget_crossed_during_final_read() {
        use std::cell::Cell;

        for origin in [node(1, 0), node(2, 1)] {
            let root = node(1, 0);
            let exhausted = Cell::new(false);
            let mut reads = 0;
            let expected_reads = if origin == root { 2 } else { 4 };
            assert_eq!(
                resolve_ancestry(
                    origin,
                    root,
                    |pid| {
                        reads += 1;
                        if reads == expected_reads {
                            exhausted.set(true);
                        }
                        Some(node(pid, pid - 1))
                    },
                    || exhausted.get(),
                ),
                Err(ProcessAncestryUnavailable::LimitExceeded),
            );
            assert_eq!(reads, expected_reads);
        }
    }

    /// Live native parent/start records agree with existing kernel start tokens.
    /// The exact current process can be reobserved without any child/helper launch.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn ancestry_live_native_record_matches_existing_identity() {
        let pid = std::process::id();
        let current = process_parent_identity_for_pid(pid).unwrap();
        assert_eq!(current.process_id, pid);
        assert_eq!(
            Some(current.start_token),
            super::super::process_metadata::process_start_token_for_pid(pid)
        );
        assert_eq!(process_ancestry(current, current).unwrap().chain, [current]);
        assert!(process_parent_identity_for_pid(0).is_none());
        assert!(process_parent_identity_for_pid(u32::MAX).is_none());
    }
}
