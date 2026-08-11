//! Managed startup compatibility for interactive zsh pane processes.
//!
//! Zsh records an interactive line before executing it when immediate or
//! shared history is enabled. This module installs a pane-scoped startup shim
//! that rejects only Mezzanine's authenticated history-control record. The
//! record can then enter a private `fc -p` history context before any generated
//! transport lines are submitted. User startup files remain owned by the user:
//! the shim sources the original `.zshenv`, `.zprofile`, `.zshrc`, and
//! `.zlogin` exactly once, preserves an effective custom `ZDOTDIR`, and wraps
//! rather than discards an existing `zshaddhistory` function.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use mez_agent::MarkerToken;
#[cfg(test)]
use mez_agent::{ShellClassification, agent_subshell_enter_command_with_zsh_history_token};
use mez_mux::process::PaneProcessLaunch;

use super::{MezError, Result};
use crate::error::MezErrorKind;

const MANAGED_ZSHENV: &str = r#"# Mezzanine-managed zsh startup compatibility.
typeset -g MEZ_ZSH_MANAGED_ZDOTDIR=${ZDOTDIR}
typeset -g MEZ_ZSH_USER_ZDOTDIR_WAS_SET=${MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET:-0}
if [[ ${MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET:-0} == 1 ]]; then
  ZDOTDIR=${MEZ_ZSH_ORIGINAL_ZDOTDIR}
else
  unset ZDOTDIR
fi
typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
if [[ -r ${MEZ_ZSH_USER_ZDOTDIR}/.zshenv ]]; then
  builtin source -- ${MEZ_ZSH_USER_ZDOTDIR}/.zshenv
fi
typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
if [[ -o RCS ]]; then
  ZDOTDIR=${MEZ_ZSH_MANAGED_ZDOTDIR}
else
  if (( ${+functions[zshaddhistory]} )); then
    functions[__mez_user_zshaddhistory]=$functions[zshaddhistory]
  fi
  function zshaddhistory() {
    emulate -L zsh
    local mez_line=${1%$'\n'}
    # ZLE passes parsed assignments without quote characters, while pipe input
    # retains the original single-quoted assignment text.
    local mez_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE=${MEZ_ZSH_HISTORY_TOKEN}; printf '\036'"
    local mez_quoted_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE='${MEZ_ZSH_HISTORY_TOKEN}'; printf '\036'"
    if [[ -n ${MEZ_ZSH_HISTORY_TOKEN-} && ( ${mez_line} == ${mez_expected} || ${mez_line} == ${mez_quoted_expected} ) ]]; then
      return 1
    fi
    if (( ${+functions[__mez_user_zshaddhistory]} )); then
      __mez_user_zshaddhistory "$@"
      return $?
    fi
    return 0
  }
  ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
fi
if [[ ${MEZ_ZSH_PRESERVE_STARTUP_CONTEXT:-0} == 1 ]]; then
  unset MEZ_ZSH_PRESERVE_STARTUP_CONTEXT
else
  unset MEZ_ZSH_ORIGINAL_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET
fi
"#;

const MANAGED_ZSHRC: &str = r#"# Mezzanine-managed zsh interactive startup compatibility.
ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
typeset -g MEZ_ZSH_SYSTEM_HISTFILE_WAS_SET=${+HISTFILE}
typeset -g MEZ_ZSH_SYSTEM_HISTFILE=${HISTFILE-}
if [[ -r ${ZDOTDIR}/.zshrc ]]; then
  builtin source -- ${ZDOTDIR}/.zshrc
fi
typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
if [[ ${MEZ_ZSH_SYSTEM_HISTFILE_WAS_SET} == 1 && \
      ${HISTFILE-} == ${MEZ_ZSH_SYSTEM_HISTFILE} && \
      ${HISTFILE} == "${MEZ_ZSH_MANAGED_ZDOTDIR}"/* ]]; then
  HISTFILE="${MEZ_ZSH_USER_ZDOTDIR}${HISTFILE:${#MEZ_ZSH_MANAGED_ZDOTDIR}}"
fi
unset MEZ_ZSH_SYSTEM_HISTFILE MEZ_ZSH_SYSTEM_HISTFILE_WAS_SET
if (( ${+functions[zshaddhistory]} )); then
  functions[__mez_user_zshaddhistory]=$functions[zshaddhistory]
else
  unfunction __mez_user_zshaddhistory 2>/dev/null || true
fi
function zshaddhistory() {
  emulate -L zsh
  local mez_line=${1%$'\n'}
  # ZLE passes parsed assignments without quote characters, while pipe input
  # retains the original single-quoted assignment text.
  local mez_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE=${MEZ_ZSH_HISTORY_TOKEN}; printf '\036'"
  local mez_quoted_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE='${MEZ_ZSH_HISTORY_TOKEN}'; printf '\036'"
  if [[ -n ${MEZ_ZSH_HISTORY_TOKEN-} && ( ${mez_line} == ${mez_expected} || ${mez_line} == ${mez_quoted_expected} ) ]]; then
    return 1
  fi
  if (( ${+functions[__mez_user_zshaddhistory]} )); then
    __mez_user_zshaddhistory "$@"
    return $?
  fi
  return 0
}
functions[__mez_zshaddhistory_guard]=$functions[zshaddhistory]
ZDOTDIR=${MEZ_ZSH_MANAGED_ZDOTDIR}
"#;

const MANAGED_ZPROFILE: &str = r#"# Mezzanine-managed zsh login startup compatibility.
ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
if [[ -r ${ZDOTDIR}/.zprofile ]]; then
  builtin source -- ${ZDOTDIR}/.zprofile
fi
typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
ZDOTDIR=${MEZ_ZSH_MANAGED_ZDOTDIR}
"#;

const MANAGED_ZLOGIN: &str = r#"# Mezzanine-managed zsh login completion compatibility.
ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
if [[ -r ${ZDOTDIR}/.zlogin ]]; then
  builtin source -- ${ZDOTDIR}/.zlogin
fi
typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
if (( ${+functions[__mez_zshaddhistory_guard]} )) && \
   [[ ${functions[zshaddhistory]-} == ${functions[__mez_zshaddhistory_guard]} ]]; then
  :
elif (( ${+functions[zshaddhistory]} )); then
  functions[__mez_user_zshaddhistory]=$functions[zshaddhistory]
else
  unfunction __mez_user_zshaddhistory 2>/dev/null || true
fi
function zshaddhistory() {
  emulate -L zsh
  local mez_line=${1%$'\n'}
  # ZLE passes parsed assignments without quote characters, while pipe input
  # retains the original single-quoted assignment text.
  local mez_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE=${MEZ_ZSH_HISTORY_TOKEN}; printf '\036'"
  local mez_quoted_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE='${MEZ_ZSH_HISTORY_TOKEN}'; printf '\036'"
  if [[ -n ${MEZ_ZSH_HISTORY_TOKEN-} && ( ${mez_line} == ${mez_expected} || ${mez_line} == ${mez_quoted_expected} ) ]]; then
    return 1
  fi
  if (( ${+functions[__mez_user_zshaddhistory]} )); then
    __mez_user_zshaddhistory "$@"
    return $?
  fi
  return 0
}
functions[__mez_zshaddhistory_guard]=$functions[zshaddhistory]
ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
unset MEZ_ZSH_MANAGED_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET
"#;

/// Pane-scoped zsh compatibility state retained for the shell lifetime.
#[derive(Debug)]
pub(super) struct ManagedZshCompatibility {
    token: MarkerToken,
    directory: PathBuf,
    original_zdotdir: Option<OsString>,
}

impl ManagedZshCompatibility {
    /// Creates private startup files beside the session's private socket.
    pub(super) fn create(
        socket_path: &Path,
        pane_id: &str,
        token: MarkerToken,
        original_zdotdir: Option<OsString>,
    ) -> Result<Self> {
        let parent = socket_path.parent().ok_or_else(|| {
            MezError::invalid_state("control socket has no parent for managed zsh startup")
        })?;
        let pane_component = pane_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let token_component = &token.as_str()[..16];
        let directory = parent.join(format!(".mez-zsh-{pane_component}-{token_component}"));
        fs::create_dir(&directory).map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to create managed zsh startup directory `{}`: {error}",
                    directory.display()
                ),
            )
        })?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            let _ = fs::remove_dir_all(&directory);
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to restrict managed zsh startup directory `{}`: {error}",
                    directory.display()
                ),
            )
        })?;
        if let Err(error) = write_private_file(&directory.join(".zshenv"), MANAGED_ZSHENV)
            .and_then(|()| write_private_file(&directory.join(".zprofile"), MANAGED_ZPROFILE))
            .and_then(|()| write_private_file(&directory.join(".zshrc"), MANAGED_ZSHRC))
            .and_then(|()| write_private_file(&directory.join(".zlogin"), MANAGED_ZLOGIN))
        {
            let _ = fs::remove_dir_all(&directory);
            return Err(error);
        }
        Ok(Self {
            token,
            directory,
            original_zdotdir,
        })
    }

    /// Adds the managed startup directory and authentication state to launch.
    pub(super) fn configure_launch(&self, launch: PaneProcessLaunch) -> PaneProcessLaunch {
        let launch = launch
            .with_interactive_arguments(["-l", "-i"])
            .with_environment_variable("ZDOTDIR", self.directory.as_os_str())
            .with_environment_variable("MEZ_ZSH_HISTORY_TOKEN", self.token.as_str())
            .with_environment_variable(
                "MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET",
                if self.original_zdotdir.is_some() {
                    "1"
                } else {
                    "0"
                },
            );
        if let Some(zdotdir) = self.original_zdotdir.as_ref() {
            launch.with_environment_variable("MEZ_ZSH_ORIGINAL_ZDOTDIR", zdotdir)
        } else {
            launch
        }
    }

    /// Returns the token recognized by this pane's startup history guard.
    pub(super) fn token(&self) -> &MarkerToken {
        &self.token
    }

    #[cfg(test)]
    /// Returns the managed startup directory for permission and cleanup tests.
    pub(super) fn directory(&self) -> &Path {
        &self.directory
    }
}

impl Drop for ManagedZshCompatibility {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// Creates one owner-only startup file without following an existing file.
fn write_private_file(path: &Path, contents: &str) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| {
            MezError::new(
                MezErrorKind::Io,
                format!(
                    "failed to create managed zsh startup file `{}`: {error}",
                    path.display()
                ),
            )
        })?;
    file.write_all(contents.as_bytes()).map_err(|error| {
        MezError::new(
            MezErrorKind::Io,
            format!(
                "failed to write managed zsh startup file `{}`: {error}",
                path.display()
            ),
        )
    })?;
    file.sync_all().map_err(|error| {
        MezError::new(
            MezErrorKind::Io,
            format!(
                "failed to sync managed zsh startup file `{}`: {error}",
                path.display()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mez_mux::process::pane_command_plan;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::Duration;

    /// Verifies managed startup artifacts are owner-only and removed with the
    /// pane-scoped compatibility owner.
    #[test]
    fn managed_zsh_compatibility_restricts_and_cleans_artifacts() {
        let root = std::env::temp_dir().join(format!(
            "mez-zsh-compat-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let socket = root.join("control.sock");
        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let compatibility = ManagedZshCompatibility::create(&socket, "%1", token, None).unwrap();
        let directory = compatibility.directory().to_path_buf();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for file in [
            directory.join(".zshenv"),
            directory.join(".zprofile"),
            directory.join(".zshrc"),
            directory.join(".zlogin"),
        ] {
            assert_eq!(
                fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(compatibility);
        assert!(!directory.exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies zsh immediate and shared history retain user commands while
    /// omitting the authenticated control record and complete Mez transport.
    ///
    /// The test uses an interactive zsh process with isolated startup and
    /// history files. It also installs a user `zshaddhistory` function to prove
    /// the managed guard composes with existing user history policy.
    #[test]
    fn managed_zsh_compatibility_isolates_immediate_and_shared_history() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        for history_option in ["INC_APPEND_HISTORY", "SHARE_HISTORY"] {
            let root = std::env::temp_dir().join(format!(
                "mez-zsh-history-{}-{history_option}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let home = root.join("home");
            let user_zdotdir = root.join("user-zdotdir");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&user_zdotdir).unwrap();
            let history = home.join("history");
            let hook_log = home.join("user-hook.log");
            fs::write(
                user_zdotdir.join(".zshenv"),
                "print -r -- zshenv >> \"$HOME/startup.log\"\n",
            )
            .unwrap();
            fs::write(
                user_zdotdir.join(".zprofile"),
                format!(
                    "print -r -- zprofile >> \"$HOME/startup.log\"\n\
HISTFILE={}\n\
HISTSIZE=200\n\
SAVEHIST=200\n\
setopt {history_option}\n\
PS1=\n",
                    shell_single_quote_path(&history),
                ),
            )
            .unwrap();
            fs::write(
                user_zdotdir.join(".zshrc"),
                format!(
                    "print -r -- zshrc >> \"$HOME/startup.log\"\n\
function zshaddhistory() {{\n\
  print -r -- \"${{1%$'\\n'}}\" >> {}\n\
  return 0\n\
}}\n",
                    shell_single_quote_path(&hook_log),
                ),
            )
            .unwrap();
            fs::write(
                user_zdotdir.join(".zlogin"),
                format!(
                    "print -r -- zlogin >> \"$HOME/startup.log\"\n\
function zshaddhistory() {{\n\
  print -r -- \"${{1%$'\\n'}}\" >> {}\n\
  return 0\n\
}}\n",
                    shell_single_quote_path(&hook_log),
                ),
            )
            .unwrap();

            let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
            let compatibility = ManagedZshCompatibility::create(
                &root.join("control.sock"),
                "%1",
                token.clone(),
                Some(user_zdotdir.as_os_str().to_os_string()),
            )
            .unwrap();
            let launch = compatibility.configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()));
            let transaction = mez_agent::ShellTransaction::new(
                MarkerToken::new("fedcba9876543210fedcba9876543210").unwrap(),
                "t1",
                "a1",
                "p1",
                zsh,
                "print -r -- AGENT_SENTINEL",
            )
            .unwrap()
            .with_zsh_history_token(token.clone());
            let input =
                transaction.render_for_classification_input(mez_agent::ShellClassification::Zsh);
            let command_plan = pane_command_plan(&launch, None).unwrap();
            assert_eq!(command_plan.args, ["-l", "-i"]);

            let mut command = Command::new(command_plan.program);
            command
                .arg("-d")
                .args(command_plan.args)
                .env("HOME", &home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (key, value) in launch.environment() {
                command.env(key, value);
            }
            let mut child = command.spawn().unwrap();
            let stdin = child.stdin.as_mut().unwrap();
            stdin.write_all(b"print -r -- USER_BEFORE\n").unwrap();
            stdin.write_all(input.wrapper.as_bytes()).unwrap();
            thread::sleep(Duration::from_millis(50));
            stdin.write_all(input.payload.as_bytes()).unwrap();
            stdin
                .write_all(
                    b"print -r -- USER_AFTER\nprint -r -- __HISTORY_BEGIN__\nfc -l -100\nprint -r -- __HISTORY_END__\nexit\n",
                )
                .unwrap();
            drop(child.stdin.take());
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "option={history_option} stdout={stdout:?} stderr={stderr:?}"
            );

            let persisted = fs::read_to_string(&history).unwrap();
            let in_memory = stdout
                .split_once("__HISTORY_BEGIN__\n")
                .and_then(|(_, tail)| tail.split_once("__HISTORY_END__"))
                .map(|(history, _)| history)
                .unwrap_or_default();
            for observed in [&persisted, in_memory] {
                assert!(
                    observed.contains("USER_BEFORE"),
                    "{history_option}: {observed}"
                );
                assert!(
                    observed.contains("USER_AFTER"),
                    "{history_option}: {observed}"
                );
                assert!(
                    !observed.contains("AGENT_SENTINEL"),
                    "{history_option}: {observed}"
                );
                assert!(!observed.contains("MEZ_"), "{history_option}: {observed}");
                assert!(
                    !observed.contains(token.as_str()),
                    "{history_option}: {observed}"
                );
                assert!(!observed.contains("fc -p"), "{history_option}: {observed}");
            }
            let restarted_command_plan = pane_command_plan(&launch, None).unwrap();
            let mut restarted_command = Command::new(restarted_command_plan.program);
            restarted_command
                .arg("-d")
                .args(restarted_command_plan.args)
                .env("HOME", &home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (key, value) in launch.environment() {
                restarted_command.env(key, value);
            }
            let mut restarted_child = restarted_command.spawn().unwrap();
            let restarted_stdin = restarted_child.stdin.as_mut().unwrap();
            restarted_stdin
                .write_all(
                    b"print -r -- USER_RESTART\nprint -r -- __RESTART_HISTORY_BEGIN__\nfc -l -100\nprint -r -- __RESTART_HISTORY_END__\nexit\n",
                )
                .unwrap();
            drop(restarted_child.stdin.take());
            let restarted_output = restarted_child.wait_with_output().unwrap();
            let restarted_stdout = String::from_utf8_lossy(&restarted_output.stdout);
            let restarted_stderr = String::from_utf8_lossy(&restarted_output.stderr);
            assert!(
                restarted_output.status.success(),
                "option={history_option} stdout={restarted_stdout:?} stderr={restarted_stderr:?}"
            );
            let restarted_history = restarted_stdout
                .split_once("__RESTART_HISTORY_BEGIN__\n")
                .and_then(|(_, tail)| tail.split_once("__RESTART_HISTORY_END__"))
                .map(|(history, _)| history)
                .unwrap_or_default();
            assert!(
                restarted_history.contains("USER_BEFORE"),
                "{history_option}: {restarted_history}"
            );
            assert!(
                restarted_history.contains("USER_AFTER"),
                "{history_option}: {restarted_history}"
            );
            assert!(
                restarted_history.contains("USER_RESTART"),
                "{history_option}: {restarted_history}"
            );
            assert!(
                !restarted_history.contains("AGENT_SENTINEL"),
                "{history_option}: {restarted_history}"
            );
            assert!(
                !restarted_history.contains("MEZ_"),
                "{history_option}: {restarted_history}"
            );
            let user_hook_lines = fs::read_to_string(&hook_log).unwrap();
            assert!(user_hook_lines.contains("USER_BEFORE"), "{user_hook_lines}");
            assert!(user_hook_lines.contains("USER_AFTER"), "{user_hook_lines}");
            let authenticated_control_record = format!(
                "fc -p && MEZ_ZSH_HISTORY_ACTIVE='{}'; printf '\\036'",
                token.as_str()
            );
            assert!(
                !user_hook_lines
                    .lines()
                    .any(|line| line == authenticated_control_record),
                "{user_hook_lines}"
            );
            assert_eq!(
                fs::read_to_string(home.join("startup.log")).unwrap(),
                "zshenv\nzprofile\nzshrc\nzlogin\nzshenv\nzprofile\nzshrc\nzlogin\n"
            );
            drop(compatibility);
            fs::remove_dir_all(root).unwrap();
        }
    }

    /// Verifies a system-provided history path derived from the managed
    /// `ZDOTDIR` is rebased to the user's startup directory and remains
    /// available when a later regular pane uses a different managed shim.
    ///
    /// macOS `/etc/zshrc` configures `HISTFILE` from the effective `ZDOTDIR`.
    /// Supplying that state through the environment keeps this regression
    /// deterministic on Linux while distinct compatibility owners model two
    /// independent Mez sessions.
    #[test]
    fn managed_zsh_compatibility_preserves_system_history_across_regular_panes() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root =
            std::env::temp_dir().join(format!("mez-zsh-regular-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let user_zdotdir = root.join("user-zdotdir");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_zdotdir).unwrap();
        fs::write(user_zdotdir.join(".zshrc"), "PS1=\n").unwrap();
        let user_history = user_zdotdir.join(".zsh_history");

        let first_compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let first_managed_history = first_compatibility.directory().join(".zsh_history");
        let first_launch =
            first_compatibility.configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()));
        let first_command_plan = pane_command_plan(&first_launch, None).unwrap();
        let mut first_command = Command::new(first_command_plan.program);
        first_command
            .arg("-d")
            .args(first_command_plan.args)
            .env("HOME", &home)
            .env("HISTFILE", &first_managed_history)
            .env("HISTSIZE", "200")
            .env("SAVEHIST", "200")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in first_launch.environment() {
            first_command.env(key, value);
        }
        let mut first_child = first_command.spawn().unwrap();
        first_child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"print -r -- REGULAR_SESSION_ONE\nexit\n")
            .unwrap();
        drop(first_child.stdin.take());
        let first_output = first_child.wait_with_output().unwrap();
        assert!(
            first_output.status.success(),
            "stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&first_output.stdout),
            String::from_utf8_lossy(&first_output.stderr)
        );
        drop(first_compatibility);

        let second_compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%2",
            MarkerToken::new("fedcba9876543210fedcba9876543210").unwrap(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let second_managed_history = second_compatibility.directory().join(".zsh_history");
        let second_launch =
            second_compatibility.configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()));
        let second_command_plan = pane_command_plan(&second_launch, None).unwrap();
        let mut second_command = Command::new(second_command_plan.program);
        second_command
            .arg("-d")
            .args(second_command_plan.args)
            .env("HOME", &home)
            .env("HISTFILE", &second_managed_history)
            .env("HISTSIZE", "200")
            .env("SAVEHIST", "200")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in second_launch.environment() {
            second_command.env(key, value);
        }
        let mut second_child = second_command.spawn().unwrap();
        second_child.stdin.as_mut().unwrap().write_all(
            b"print -r -- __REGULAR_HISTORY_BEGIN__\nfc -l -100\nprint -r -- __REGULAR_HISTORY_END__\nexit\n",
        ).unwrap();
        drop(second_child.stdin.take());
        let second_output = second_child.wait_with_output().unwrap();
        let second_stdout = String::from_utf8_lossy(&second_output.stdout);
        let second_stderr = String::from_utf8_lossy(&second_output.stderr);
        assert!(
            second_output.status.success(),
            "stdout={second_stdout:?} stderr={second_stderr:?}"
        );
        let restarted_history = second_stdout
            .split_once("__REGULAR_HISTORY_BEGIN__\n")
            .and_then(|(_, tail)| tail.split_once("__REGULAR_HISTORY_END__"))
            .map(|(history, _)| history)
            .unwrap_or_default();
        assert!(
            restarted_history.contains("REGULAR_SESSION_ONE"),
            "{restarted_history}"
        );
        let persisted = fs::read_to_string(&user_history).unwrap();
        assert!(persisted.contains("REGULAR_SESSION_ONE"), "{persisted}");
        assert!(!first_managed_history.exists());
        assert!(!second_managed_history.exists());

        drop(second_compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies a persistent agent child preserves the user-configured zsh
    /// history file after its private Mez handoff frame closes. The child must
    /// retain ordinary commands while neither the authenticated control record
    /// nor generated transport source reaches memory or the saved history.
    #[test]
    fn persistent_agent_zsh_child_persists_user_history_after_handoff() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "mez-zsh-agent-child-history-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let user_zdotdir = root.join("user-zdotdir");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_zdotdir).unwrap();
        let history = home.join("history");
        fs::write(
            user_zdotdir.join(".zshrc"),
            format!(
                "HISTFILE={}\nHISTSIZE=200\nSAVEHIST=200\nsetopt INC_APPEND_HISTORY\nPS1=\n",
                shell_single_quote_path(&history),
            ),
        )
        .unwrap();

        let token = MarkerToken::new("0123456789abcdef0123456789abcdef").unwrap();
        let compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            token.clone(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let launch = compatibility.configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()));
        let handoff = agent_subshell_enter_command_with_zsh_history_token(
            zsh,
            ShellClassification::Zsh,
            Some(&token),
        )
        .unwrap();
        let agent_transaction = mez_agent::ShellTransaction::new(
            MarkerToken::new("fedcba9876543210fedcba9876543210").unwrap(),
            "t1",
            "a1",
            "p1",
            zsh,
            "print -r -- AGENT_CHILD_SENTINEL",
        )
        .unwrap()
        .with_zsh_history_token(token.clone())
        .render_for_classification_input(ShellClassification::Zsh);

        let mut command = Command::new(launch.program());
        command
            .args(["-d", "-i"])
            .env("HOME", &home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in launch.environment() {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(handoff.as_bytes()).unwrap();
        stdin
            .write_all(agent_transaction.wrapper.as_bytes())
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        stdin
            .write_all(agent_transaction.payload.as_bytes())
            .unwrap();
        stdin.write_all(b"print -r -- USER_CHILD\n").unwrap();
        stdin.write_all(b"print -r -- __HISTORY_BEGIN__\nfc -l -100\nprint -r -- __HISTORY_END__\nexit\nexit\n").unwrap();
        drop(child.stdin.take());
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stdout={stdout:?} stderr={stderr:?}"
        );

        let persisted = fs::read_to_string(&history).unwrap();
        let in_memory = stdout
            .split_once("__HISTORY_BEGIN__\n")
            .and_then(|(_, tail)| tail.split_once("__HISTORY_END__"))
            .map(|(history, _)| history)
            .unwrap_or_default();
        for observed in [&persisted, in_memory] {
            assert!(observed.contains("USER_CHILD"), "{observed}");
            assert!(!observed.contains("AGENT_CHILD_SENTINEL"), "{observed}");
            assert!(!observed.contains("MEZ_"), "{observed}");
            assert!(!observed.contains(token.as_str()), "{observed}");
            assert!(!observed.contains("fc -p"), "{observed}");
        }

        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Quotes one test path as a literal zsh word.
    fn shell_single_quote_path(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
    }
}
