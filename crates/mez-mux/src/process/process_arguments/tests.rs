//! Private synthetic argument layouts and one real native self observation.

use super::*;

/// Native argv preserves empty and non-UTF-8 strings without text coercion.
/// A missing final separator, oversize input or argument-count overflow must
/// produce unknown rather than a prefix that could hide an unsafe mode flag.
#[test]
fn native_arguments_nul_layout_preserves_bytes_and_rejects_truncation() {
    assert_eq!(
        parse_nul_arguments(b"alias\0\0\xff\0"),
        Some(vec![b"alias".to_vec(), vec![], vec![255]])
    );
    assert!(parse_nul_arguments(b"alias\0--listen").is_none());
    assert!(parse_nul_arguments(b"").is_none());
    assert!(parse_nul_arguments(&vec![0; MAX_BYTES + 1]).is_none());
    assert!(parse_nul_arguments(&vec![0; MAX_ARGUMENTS + 1]).is_none());
}

/// A refusal query must not mistake padding for proof of a nonempty argv[0].
/// Every feasible layout is considered, including multiple leading empty args;
/// only a Boolean is returned, and protected environment hypotheses are never
/// exported as an allegedly exact argument vector. Known shared flags cannot
/// disappear through alignment, while ordinary vectors remain non-refusing.
#[test]
fn native_arguments_darwin_layout_checks_all_empty_prefix_hypotheses() {
    fn shared(args: &[Vec<u8>]) -> bool {
        args.iter().any(|arg| arg == b"app-server")
            && args.iter().any(|arg| arg == b"--managed-daemon")
    }
    for padding in 0..4 {
        for empty in 0..4 {
            for unsafe_mode in [false, true] {
                let arguments: &[&[u8]] = if unsafe_mode {
                    &[b"alias", b"app-server", b"--managed-daemon"]
                } else {
                    &[b"alias", b"resume", b"--last"]
                };
                let mut bytes = i32::try_from(empty + arguments.len())
                    .unwrap()
                    .to_ne_bytes()
                    .to_vec();
                bytes.extend_from_slice(b"/native/executable\0");
                bytes.extend(vec![0; padding + empty]);
                for argument in arguments {
                    bytes.extend_from_slice(argument);
                    bytes.push(0);
                }
                for _ in 0..4 {
                    bytes.extend_from_slice(b"PRIVATE_KEY=private-value\0");
                }
                assert_eq!(macos_refusal(&bytes, shared), Some(unsafe_mode));
            }
        }
    }
    for count in [-1_i32, 0, 257] {
        let mut bytes = count.to_ne_bytes().to_vec();
        bytes.extend_from_slice(b"/exe\0alias\0");
        assert!(macos_refusal(&bytes, shared).is_none());
    }
    let mut missing = 1_i32.to_ne_bytes().to_vec();
    missing.extend_from_slice(b"/exe\0unterminated");
    assert!(macos_refusal(&missing, shared).is_none());
    let mut large = 1_i32.to_ne_bytes().to_vec();
    large.extend_from_slice(b"/exe\0");
    large.extend(vec![b'x'; MAX_BYTES]);
    large.push(0);
    assert!(macos_refusal(&large, shared).is_none());
}

/// The native reader observes this real process without supplied argv metadata.
/// It must return the actual executable argument while a nonexistent PID is
/// unknown. No private argument values are printed or written by this test.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn native_arguments_self_observation_matches_actual_process() {
    fn own_executable(arguments: &[Vec<u8>]) -> bool {
        use std::os::unix::ffi::OsStrExt;
        arguments.first().is_some_and(|argument| {
            argument.as_slice() == std::env::args_os().next().unwrap().as_os_str().as_bytes()
        })
    }
    assert_eq!(
        process_argument_refusal_for_pid(std::process::id(), own_executable),
        Some(true)
    );
    assert!(process_argument_refusal_for_pid(u32::MAX, own_executable).is_none());
}
