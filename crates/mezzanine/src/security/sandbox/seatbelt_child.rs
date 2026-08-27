//! Code-owned Seatbelt workload child launcher.
//!
//! The launcher starts only through a fixed hidden Mezzanine process mode
//! after `sandbox-exec` has entered the generated profile. It validates every
//! typed path and the owner-only environment document, emits bounded lifecycle
//! records on the runtime-owned status descriptor, marks that descriptor
//! close-on-exec before spawning the payload, rebuilds the payload environment,
//! and mirrors the payload exit status. It accepts no SBPL, arbitrary launcher
//! arguments, or user-selected executable beyond the already compiled child
//! shell path.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path};
use std::process::{Command, Stdio};

use serde::Serialize;

use super::SANDBOX_STATUS_FD;

/// Fixed outer mode selected only by the code-owned Seatbelt compiler.
pub(crate) const INTERNAL_LAUNCH_ARGUMENT: &str = "--mez-internal-seatbelt-launch";
/// Fixed inner mode entered only after `sandbox-exec` applies the profile.
pub(crate) const INTERNAL_CHILD_ARGUMENT: &str = "--mez-internal-seatbelt-child";
const ENVIRONMENT_DOCUMENT_MAX_BYTES: u64 = 64 * 1024;
const ENVIRONMENT_MAX_ENTRIES: usize = 128;
const ENVIRONMENT_NAME_MAX_BYTES: usize = 128;
const LAUNCHER_FAILURE_EXIT_CODE: u8 = 125;

/// Dispatches the exact hidden Seatbelt child-launch mode from process argv.
///
/// Ordinary CLI invocations return `None`. Malformed internal invocations fail
/// closed without entering normal CLI initialization.
pub(crate) fn run_internal_process(arguments: &[OsString]) -> Option<u8> {
    let mode = arguments.get(1)?.to_str()?;
    let result = match mode {
        INTERNAL_LAUNCH_ARGUMENT if arguments.len() == 10 => run_outer(
            Path::new(&arguments[2]),
            Path::new(&arguments[3]),
            Path::new(&arguments[4]),
            Path::new(&arguments[5]),
            Path::new(&arguments[6]),
            Path::new(&arguments[7]),
            Path::new(&arguments[8]),
            Path::new(&arguments[9]),
        ),
        INTERNAL_CHILD_ARGUMENT if arguments.len() == 8 => run_child(
            Path::new(&arguments[2]),
            Path::new(&arguments[3]),
            Path::new(&arguments[4]),
            Path::new(&arguments[5]),
            Path::new(&arguments[6]),
            Path::new(&arguments[7]),
        ),
        INTERNAL_LAUNCH_ARGUMENT | INTERNAL_CHILD_ARGUMENT => Err("invalid-arguments"),
        _ => return None,
    };
    let exit_code = result.unwrap_or_else(|failure| {
        eprintln!("mez: internal Seatbelt child launch failed ({failure})");
        LAUNCHER_FAILURE_EXIT_CODE
    });
    Some(exit_code)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the fixed internal launcher carries one exact typed path per workload artifact"
)]
fn run_outer(
    sandbox_executable: &Path,
    profile_file: &Path,
    working_directory: &Path,
    home_directory: &Path,
    temporary_directory: &Path,
    child_shell: &Path,
    command_file: &Path,
    environment_file: &Path,
) -> Result<u8, &'static str> {
    if sandbox_executable != Path::new("/usr/bin/sandbox-exec") {
        return Err("sandbox-executable");
    }
    validate_executable(sandbox_executable)?;
    validate_private_file(profile_file)?;
    validate_directory(working_directory, false)?;
    validate_directory(home_directory, true)?;
    validate_directory(temporary_directory, true)?;
    validate_executable(child_shell)?;
    validate_private_file(command_file)?;
    validate_private_file(environment_file)?;
    let current_executable =
        fs::canonicalize(std::env::current_exe().map_err(|_| "launcher-path")?)
            .map_err(|_| "launcher-path")?;
    let (status_reader, status_writer) = UnixStream::pair().map_err(|_| "status-pipe")?;
    let writer_fd = status_writer.as_raw_fd();
    let mut command = Command::new(sandbox_executable);
    command
        .arg("-f")
        .arg(profile_file)
        .arg(&current_executable)
        .arg(INTERNAL_CHILD_ARGUMENT)
        .arg(working_directory)
        .arg(home_directory)
        .arg(temporary_directory)
        .arg(child_shell)
        .arg(command_file)
        .arg(environment_file)
        .env_clear()
        .env("HOME", home_directory)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("PATH", "/usr/bin:/bin")
        .env("TMPDIR", temporary_directory)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    // SAFETY: the closure performs only descriptor duplication between fork
    // and exec. The socket writer remains alive until `spawn` returns.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(writer_fd, i32::from(SANDBOX_STATUS_FD)) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::fcntl(i32::from(SANDBOX_STATUS_FD), libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|_| "sandbox-spawn")?;
    drop(status_writer);
    let exit = child.wait().map_err(|_| "sandbox-wait")?;
    let mut status_bytes = Vec::new();
    status_reader
        .take(ENVIRONMENT_DOCUMENT_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut status_bytes)
        .map_err(|_| "status-read")?;
    if u64::try_from(status_bytes.len()).unwrap_or(u64::MAX) > ENVIRONMENT_DOCUMENT_MAX_BYTES {
        return Err("status-size");
    }
    // SAFETY: the typed parent launch installs the runtime-owned descriptor at
    // this fixed number before executing the outer supervisor.
    let mut outer_status = unsafe { File::from_raw_fd(i32::from(SANDBOX_STATUS_FD)) };
    outer_status
        .write_all(&status_bytes)
        .and_then(|()| outer_status.flush())
        .map_err(|_| "status-forward")?;
    Ok(exit_status_code(exit))
}

fn exit_status_code(exit: std::process::ExitStatus) -> u8 {
    exit.code()
        .or_else(|| exit.signal().map(|signal| 128_i32.saturating_add(signal)))
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(LAUNCHER_FAILURE_EXIT_CODE)
}

fn run_child(
    working_directory: &Path,
    home_directory: &Path,
    temporary_directory: &Path,
    child_shell: &Path,
    command_file: &Path,
    environment_file: &Path,
) -> Result<u8, &'static str> {
    validate_directory(working_directory, false)?;
    validate_directory(home_directory, true)?;
    validate_directory(temporary_directory, true)?;
    validate_executable(child_shell)?;
    validate_private_file(command_file)?;
    validate_private_file(environment_file)?;
    let environment = read_environment_document(environment_file)?;
    validate_projected_environment(
        &environment,
        home_directory,
        temporary_directory,
        child_shell,
    )?;

    // SAFETY: the typed outer launch installs the runtime-owned descriptor at
    // this fixed number before exec. This process takes ownership exactly once.
    let mut status = unsafe { File::from_raw_fd(i32::from(SANDBOX_STATUS_FD)) };
    write_status(
        &mut status,
        &SeatbeltStatusRecord::SandboxEntered {
            version: 1,
            event: "sandbox-entered",
        },
    )?;
    mark_close_on_exec(&status)?;

    let mut child = match Command::new(child_shell)
        .arg(command_file)
        .current_dir(working_directory)
        .env_clear()
        .envs(&environment)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => {
            write_status(
                &mut status,
                &SeatbeltStatusRecord::Failure {
                    version: 1,
                    event: "spawn-failed",
                    code: "payload-spawn",
                },
            )?;
            return Ok(LAUNCHER_FAILURE_EXIT_CODE);
        }
    };
    write_status(
        &mut status,
        &SeatbeltStatusRecord::ChildEstablished {
            version: 1,
            event: "child-established",
            child_pid: child.id(),
        },
    )?;
    let exit_code = match child.wait() {
        Ok(exit) => exit_status_code(exit),
        Err(_) => {
            write_status(
                &mut status,
                &SeatbeltStatusRecord::Failure {
                    version: 1,
                    event: "wait-failed",
                    code: "payload-wait",
                },
            )?;
            return Ok(LAUNCHER_FAILURE_EXIT_CODE);
        }
    };
    write_status(
        &mut status,
        &SeatbeltStatusRecord::Exited {
            version: 1,
            event: "exit",
            exit_code: i32::from(exit_code),
        },
    )?;
    Ok(exit_code)
}

#[derive(Serialize)]
#[serde(untagged)]
enum SeatbeltStatusRecord<'a> {
    SandboxEntered {
        version: u8,
        event: &'a str,
    },
    ChildEstablished {
        version: u8,
        event: &'a str,
        #[serde(rename = "child-pid")]
        child_pid: u32,
    },
    Exited {
        version: u8,
        event: &'a str,
        #[serde(rename = "exit-code")]
        exit_code: i32,
    },
    Failure {
        version: u8,
        event: &'a str,
        code: &'a str,
    },
}

fn write_status(status: &mut File, record: &SeatbeltStatusRecord<'_>) -> Result<(), &'static str> {
    serde_json::to_writer(&mut *status, record).map_err(|_| "status-encode")?;
    status.write_all(b"\n").map_err(|_| "status-write")?;
    status.flush().map_err(|_| "status-write")
}

fn mark_close_on_exec(status: &File) -> Result<(), &'static str> {
    let fd = status.as_raw_fd();
    // SAFETY: `fd` is the live status file descriptor owned by this process.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err("status-flags");
    }
    // SAFETY: setting FD_CLOEXEC changes only descriptor inheritance state.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err("status-flags");
    }
    Ok(())
}

fn validate_directory(path: &Path, require_private: bool) -> Result<(), &'static str> {
    validate_absolute_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "path-inspection")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("directory-kind");
    }
    if require_private && metadata.permissions().mode() & 0o077 != 0 {
        return Err("directory-mode");
    }
    Ok(())
}

fn validate_executable(path: &Path) -> Result<(), &'static str> {
    validate_absolute_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "shell-inspection")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err("shell-kind");
    }
    Ok(())
}

fn validate_private_file(path: &Path) -> Result<(), &'static str> {
    validate_absolute_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "file-inspection")?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("file-kind");
    }
    Ok(())
}

fn validate_absolute_path(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
    {
        return Err("path-syntax");
    }
    Ok(())
}

fn read_environment_document(path: &Path) -> Result<BTreeMap<String, String>, &'static str> {
    let metadata = fs::metadata(path).map_err(|_| "environment-read")?;
    if metadata.size() > ENVIRONMENT_DOCUMENT_MAX_BYTES {
        return Err("environment-size");
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.size()).unwrap_or(0));
    File::open(path)
        .map_err(|_| "environment-read")?
        .take(ENVIRONMENT_DOCUMENT_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| "environment-read")?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > ENVIRONMENT_DOCUMENT_MAX_BYTES {
        return Err("environment-size");
    }
    let environment = serde_json::from_slice::<BTreeMap<String, String>>(&bytes)
        .map_err(|_| "environment-format")?;
    if environment.len() > ENVIRONMENT_MAX_ENTRIES
        || environment
            .iter()
            .any(|(name, value)| !valid_environment_name(name) || value.as_bytes().contains(&0))
    {
        return Err("environment-entry");
    }
    Ok(environment)
}

fn valid_environment_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= ENVIRONMENT_NAME_MAX_BYTES
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}

fn validate_projected_environment(
    environment: &BTreeMap<String, String>,
    home_directory: &Path,
    temporary_directory: &Path,
    child_shell: &Path,
) -> Result<(), &'static str> {
    for (name, expected) in [
        ("HOME", home_directory),
        ("TMPDIR", temporary_directory),
        ("SHELL", child_shell),
    ] {
        let expected = expected.to_str().ok_or("path-encoding")?;
        if environment.get(name).map(String::as_str) != Some(expected) {
            return Err("environment-projection");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the hidden process boundary ignores ordinary CLI invocations
    /// and rejects malformed launcher argument counts before touching FD 3.
    #[test]
    fn internal_dispatch_is_exact_and_fail_closed() {
        assert_eq!(
            run_internal_process(&[OsString::from("mez"), OsString::from("version")]),
            None
        );
        assert_eq!(
            run_internal_process(&[
                OsString::from("mez"),
                OsString::from(INTERNAL_CHILD_ARGUMENT)
            ]),
            Some(LAUNCHER_FAILURE_EXIT_CODE)
        );
        assert_eq!(
            run_internal_process(&[
                OsString::from("mez"),
                OsString::from(INTERNAL_LAUNCH_ARGUMENT)
            ]),
            Some(LAUNCHER_FAILURE_EXIT_CODE)
        );
    }

    /// Verifies environment documents reject shell-invalid names and NUL
    /// values rather than forwarding ambiguous process environment entries.
    #[test]
    fn environment_entry_validation_is_strict() {
        assert!(valid_environment_name("PATH"));
        assert!(valid_environment_name("GIT_CONFIG_KEY_0"));
        assert!(!valid_environment_name("9INVALID"));
        assert!(!valid_environment_name("INVALID-NAME"));
    }
}
