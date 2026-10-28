//! Bounded negative-only native argument queries for topology diagnostics.
//!
//! Arguments can contain secrets and are mutable process data, not executable,
//! client, ancestry or request authority. Callers must discard them after local
//! classification and must not log, transport or persist them. Native lifetime
//! and relationship checks remain separate. Linux reads procfs; Darwin reads
//! KERN_PROCARGS2, which has ambiguous padding/leading-empty layouts. Every feasible
//! layout is checked conservatively; only a Boolean leaves this owner. Borrowed
//! hypothesis data can overlap environment and must not be retained or logged.

/// Maximum candidate argv bytes, including NUL separators.
const MAX_BYTES: usize = 64 * 1024;
/// Maximum candidate argument count; exhaustion is unknown, never truncation.
const MAX_ARGUMENTS: usize = 256;

/// Parses one complete procfs argv observation without decoding private bytes.
#[cfg(any(target_os = "linux", test))]
fn parse_nul_arguments(bytes: &[u8]) -> Option<Vec<Vec<u8>>> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES || bytes.last() != Some(&0) {
        return None;
    }
    let arguments = bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .take(MAX_ARGUMENTS + 1)
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    (arguments.len() <= MAX_ARGUMENTS).then_some(arguments)
}

/// Checks every feasible Darwin padding/leading-empty-argv interpretation.
/// At least one candidate contains the actual vector; any refusing candidate
/// refuses the observation. Incomplete/capacity failures stay unknown. Candidate
/// bytes are private (including a possible environment tail), never returned.
#[cfg(any(target_os = "macos", test))]
fn macos_refusal(bytes: &[u8], refuse: fn(&[Vec<u8>]) -> bool) -> Option<bool> {
    let count = i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?);
    let count = usize::try_from(count)
        .ok()
        .filter(|count| *count > 0 && *count <= MAX_ARGUMENTS)?;
    let after_path = 4 + bytes.get(4..)?.iter().position(|byte| *byte == 0)? + 1;
    let mut start = after_path;
    while bytes.get(start) == Some(&0) {
        start += 1;
    }
    let possible_empty = (start - after_path).min(count);
    let mut refused = false;
    for empty in 0..=possible_empty {
        let mut position = start;
        let mut size = empty;
        let mut arguments = vec![Vec::new(); empty];
        for _ in empty..count {
            let length = bytes.get(position..)?.iter().position(|byte| *byte == 0)?;
            let end = position.checked_add(length)?;
            size = size.checked_add(length)?.checked_add(1)?;
            if size > MAX_BYTES {
                return None;
            }
            arguments.push(bytes.get(position..end)?.to_vec());
            position = end + 1;
        }
        refused |= refuse(&arguments);
    }
    Some(refused)
}

/// Applies a trusted negative-only predicate to a complete bounded Linux argv.
/// Returns None on read/parse/capacity failure. This cannot authorize a process
/// or establish native lifetime. The predicate must not retain/log/transport its
/// protected borrowed input; only its refusal Boolean is returned. Independently
/// revalidate native identity before and after the query.
#[cfg(target_os = "linux")]
pub fn process_argument_refusal_for_pid(pid: u32, refuse: fn(&[Vec<u8>]) -> bool) -> Option<bool> {
    use std::io::Read;
    if pid == 0 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/{pid}/cmdline"))
        .ok()?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(refuse(&parse_nul_arguments(&bytes)?))
}

/// Conservatively applies a negative-only predicate to every feasible Darwin
/// argv layout. Padding and leading empty arguments cannot be distinguished, so
/// hypothesis input may overlap environment. The trusted predicate must not
/// retain/log/transport that protected input or treat it as authority. Only a
/// refusal Boolean leaves this owner; incomplete/capacity observations are None.
#[cfg(target_os = "macos")]
pub fn process_argument_refusal_for_pid(pid: u32, refuse: fn(&[Vec<u8>]) -> bool) -> Option<bool> {
    let bytes = super::process_metadata::read_macos_procargs(pid, 256 * 1024)?;
    macos_refusal(&bytes, refuse)
}

/// Returns unknown on hosts without an implemented native argument reader.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_argument_refusal_for_pid(
    _pid: u32,
    _refuse: fn(&[Vec<u8>]) -> bool,
) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests;
