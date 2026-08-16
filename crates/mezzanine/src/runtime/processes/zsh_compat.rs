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
use mez_agent::{
    ShellClassification, agent_subshell_enter_command_with_shell_compatibility_and_exit_marker,
};
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
typeset -g MEZ_ZSH_INTEGRATION_ONLY=1
builtin source -- ${MEZ_ZSH_MANAGED_ZDOTDIR}/.zlogin
unset MEZ_ZSH_INTEGRATION_ONLY
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
typeset -g MEZ_ZSH_INTEGRATION_ONLY=1
builtin source -- ${MEZ_ZSH_MANAGED_ZDOTDIR}/.zlogin
unset MEZ_ZSH_INTEGRATION_ONLY
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
if [[ -o RCS ]]; then
  ZDOTDIR=${MEZ_ZSH_MANAGED_ZDOTDIR}
else
  ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
  unset MEZ_ZSH_PRESERVE_STARTUP_CONTEXT MEZ_ZSH_MANAGED_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET
fi
"#;

const MANAGED_ZPROFILE: &str = r#"# Mezzanine-managed zsh login startup compatibility.
ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
if [[ -r ${ZDOTDIR}/.zprofile ]]; then
  builtin source -- ${ZDOTDIR}/.zprofile
fi
typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
typeset -g MEZ_ZSH_INTEGRATION_ONLY=1
builtin source -- ${MEZ_ZSH_MANAGED_ZDOTDIR}/.zlogin
unset MEZ_ZSH_INTEGRATION_ONLY
if [[ -o RCS ]]; then
  ZDOTDIR=${MEZ_ZSH_MANAGED_ZDOTDIR}
else
  if (( ${+functions[zshaddhistory]} )); then
    functions[__mez_user_zshaddhistory]=$functions[zshaddhistory]
  fi
  function zshaddhistory() {
    emulate -L zsh
    local mez_line=${1%$'\n'}
    local mez_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE=${MEZ_ZSH_HISTORY_TOKEN}; printf '\036'"
    local mez_quoted_expected="fc -p && MEZ_ZSH_HISTORY_ACTIVE='${MEZ_ZSH_HISTORY_TOKEN}'; printf '\036'"
    if [[ -n ${MEZ_ZSH_HISTORY_TOKEN-} && ( ${mez_line} == ${mez_expected} || ${mez_line} == ${mez_quoted_expected} || ${mez_line} == __mez_zsh_private_receiver ) ]]; then
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
  unset MEZ_ZSH_PRESERVE_STARTUP_CONTEXT MEZ_ZSH_MANAGED_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET
fi
"#;

const MANAGED_ZLOGIN: &str = r#"# Mezzanine-managed zsh login completion compatibility.
if [[ ${MEZ_ZSH_INTEGRATION_ONLY:-0} != 1 ]]; then
  ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
  if [[ -r ${ZDOTDIR}/.zlogin ]]; then
    builtin source -- ${ZDOTDIR}/.zlogin
  fi
  typeset -g MEZ_ZSH_USER_ZDOTDIR=${ZDOTDIR:-$HOME}
fi
if [[ ${MEZ_ZSH_INTEGRATION_ONLY:-0} != 1 ]]; then
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
    if [[ -n ${MEZ_ZSH_HISTORY_TOKEN-} && \
          ( ${mez_line} == ${mez_expected} || ${mez_line} == ${mez_quoted_expected} || \
            ${mez_line} == __mez_zsh_private_receiver ) ]]; then
      return 1
    fi
    if (( ${+functions[__mez_user_zshaddhistory]} )); then
      __mez_user_zshaddhistory "$@"
      return $?
    fi
    return 0
  }
  functions[__mez_zshaddhistory_guard]=$functions[zshaddhistory]
fi
typeset -g __MEZ_ZSH_RESTORE_MARKER=
typeset -gi __MEZ_ZSH_RESTORE_STATUS=1
typeset -g __MEZ_ZSH_RESTORE_OUTCOME=frame-rejected
typeset -gi __MEZ_ZSH_ADMISSION_READY=0
typeset -gi __MEZ_ZSH_EDITOR_CLEAR_PHASE=0
typeset -g __MEZ_ZSH_TRIGGER_ID=
typeset -g __MEZ_ZSH_TRIGGER_SEQUENCE=
function __mez_zsh_private_receiver() {
  emulate -L zsh
  setopt localoptions extendedglob
  local hold_record begin_record source_file encoded_file receive_status=0 source_status=1 cancelled=0 frame_admitted=0
  local marker expected_length expected_digest expected_chunks sequence=0
  command printf '\033]133;R;mez_receiver=awaiting;mez_token=%s\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
  IFS= read -r hold_record || receive_status=1
  local -a hold_fields
  hold_fields=(${=hold_record})
  if (( receive_status != 0 || ${#hold_fields} != 3 || ${#hold_record} > __MEZ_ZSH_MAX_RECORD_BYTES__ )) || \
     [[ ${hold_fields[1]-} != MEZ_ZSH_RX1_HOLD || ${hold_fields[2]-} != ${MEZ_ZSH_HISTORY_TOKEN-} || -z ${hold_fields[3]-} ]]; then
    command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=receiver-rejected;mez_reason=invalid-hold\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    __MEZ_ZSH_RESTORE_STATUS=65
    __MEZ_ZSH_RESTORE_OUTCOME=frame-rejected
    return 65
  fi
  marker=${hold_fields[3]}
  command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=editor-held;mez_marker=%s\033\\' \
    "$MEZ_ZSH_HISTORY_TOKEN" "$marker"
  IFS= read -r begin_record || receive_status=1
  local -a begin_fields
  begin_fields=(${=begin_record})
  if (( receive_status != 0 || ${#begin_fields} != 6 || ${#begin_record} > __MEZ_ZSH_MAX_RECORD_BYTES__ )) || \
     [[ ${begin_fields[1]-} != MEZ_ZSH_RX1_BEGIN || ${begin_fields[2]-} != ${MEZ_ZSH_HISTORY_TOKEN-} || \
        ${begin_fields[3]-} != ${marker} || \
        ${begin_fields[4]-} != <-> || ${begin_fields[5]-} != [0-9a-f]## || ${#begin_fields[5]} != 64 || \
        ${begin_fields[6]-} != <-> || ${begin_fields[4]-} -gt __MEZ_ZSH_MAX_SOURCE_BYTES__ || \
        ${begin_fields[6]-} -gt __MEZ_ZSH_MAX_SOURCE_CHUNKS__ ]]; then
    receive_status=1
  else
    expected_length=${begin_fields[4]}
    expected_digest=${begin_fields[5]}
    expected_chunks=${begin_fields[6]}
    frame_admitted=1
    command printf '\036'
    command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=frame-admitted;mez_marker=%s\033\\' \
      "$MEZ_ZSH_HISTORY_TOKEN" "$marker"
  fi
  if (( ! cancelled && frame_admitted )); then
    source_file=$(command mktemp) || receive_status=1
    encoded_file=${source_file}.b64
    (( receive_status == 0 )) && command printf '' >| "$encoded_file"
  fi
  while (( ! cancelled && frame_admitted && sequence < expected_chunks )); do
    local data_record
    IFS= read -r data_record || { receive_status=1; break; }
    local -a data_fields
    data_fields=(${=data_record})
    if (( ${#data_fields} == 3 )) && \
       [[ ${data_fields[1]-} == MEZ_ZSH_RX1_CANCEL && \
          ${data_fields[2]-} == ${MEZ_ZSH_HISTORY_TOKEN-} && ${data_fields[3]-} == ${marker} ]]; then
      if (( sequence == 0 )); then
        cancelled=1
        source_status=130
        command printf '\036'
        break
      fi
      receive_status=1
    elif (( receive_status == 0 )) && \
       { (( ${#data_fields} != 5 || ${#data_record} > __MEZ_ZSH_MAX_RECORD_BYTES__ || ${#data_fields[5]-} > __MEZ_ZSH_MAX_DATA_BYTES__ )) || \
         [[ ${data_fields[1]-} != MEZ_ZSH_RX1_DATA || \
         ${data_fields[2]-} != ${MEZ_ZSH_HISTORY_TOKEN-} || ${data_fields[3]-} != ${marker} || \
         ${data_fields[4]-} != ${sequence} || ${data_fields[5]-} != [A-Za-z0-9+/]##(|=|==) ]]; }; then
      receive_status=1
    elif (( receive_status == 0 )); then
      command printf '%s' "${data_fields[5]}" >> "$encoded_file" || receive_status=1
    fi
    (( sequence++ ))
    command printf '\036'
  done
  local end_record
  if (( ! cancelled && frame_admitted && sequence == expected_chunks )); then
    IFS= read -r end_record || receive_status=1
    local -a end_fields
    end_fields=(${=end_record})
    if (( receive_status == 0 )) && \
       { (( ${#end_fields} != 6 || ${#end_record} > __MEZ_ZSH_MAX_RECORD_BYTES__ )) || \
         [[ ${end_fields[1]-} != MEZ_ZSH_RX1_END || \
         ${end_fields[2]-} != ${MEZ_ZSH_HISTORY_TOKEN-} || ${end_fields[3]-} != ${marker} || \
         ${end_fields[4]-} != ${expected_chunks} || ${end_fields[5]-} != ${expected_length} || \
         ${end_fields[6]-} != ${expected_digest} ]]; }; then
      receive_status=1
    fi
    command printf '\036'
  fi
  if (( ! cancelled && receive_status == 0 )); then
    if command printf '' | base64 -d >/dev/null 2>&1; then
      command base64 -d < "$encoded_file" >| "$source_file" 2>/dev/null || receive_status=$?
    else
      command base64 -D < "$encoded_file" >| "$source_file" || receive_status=$?
    fi
  fi
  if (( ! cancelled && receive_status == 0 )); then
    local expected_encoded actual_encoded
    expected_encoded=$(command cat -- "$encoded_file") || receive_status=$?
    actual_encoded=$(command base64 < "$source_file" | command tr -d '\r\n') || receive_status=$?
    [[ ${actual_encoded} == ${expected_encoded} ]] || receive_status=1
  fi
  if (( ! cancelled && receive_status == 0 )); then
    local actual_length actual_digest
    actual_length=$(command wc -c < "$source_file" | command tr -d '[:space:]')
    if (( ${+commands[sha256sum]} )); then
      actual_digest=$(command sha256sum -- "$source_file" | command awk '{print $1}')
    elif (( ${+commands[shasum]} )); then
      actual_digest=$(command shasum -a 256 -- "$source_file" | command awk '{print $1}')
    else
      receive_status=127
    fi
    if (( receive_status == 0 )) && [[ ${actual_length} == ${expected_length} && ${actual_digest} == ${expected_digest} ]]; then
      builtin source "$source_file"
      source_status=$?
    fi
  fi
  [[ -n ${source_file} ]] && command rm -f -- "$source_file" "$encoded_file" >/dev/null 2>&1 || true
  __MEZ_ZSH_RESTORE_MARKER=${marker}
  __MEZ_ZSH_RESTORE_STATUS=${source_status}
  if (( cancelled )); then
    __MEZ_ZSH_RESTORE_OUTCOME=cancelled
  elif (( receive_status != 0 )); then
    __MEZ_ZSH_RESTORE_OUTCOME=frame-rejected
    __MEZ_ZSH_RESTORE_STATUS=65
  elif (( source_status == 0 )); then
    __MEZ_ZSH_RESTORE_OUTCOME=completed
  else
    __MEZ_ZSH_RESTORE_OUTCOME=source-failed
  fi
  return ${source_status}
}
function __mez_zsh_private_widget() {
  emulate -L zsh
  if [[ -n ${PREBUFFER-} ]] || (( ${KEYS_QUEUED_COUNT:-0} != 0 || ! __MEZ_ZSH_ADMISSION_READY )); then
    zle beep
    return 1
  fi
  BUFFER=
  CURSOR=0
  MARK=0
  REGION_ACTIVE=0
  if (( __MEZ_ZSH_EDITOR_CLEAR_PHASE == 0 )); then
    __MEZ_ZSH_EDITOR_CLEAR_PHASE=1
    zle redisplay
    command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=editor-clear-requested\033\\' \
      "$MEZ_ZSH_HISTORY_TOKEN"
    return 0
  fi
  if (( __MEZ_ZSH_EDITOR_CLEAR_PHASE == 1 )); then
    __MEZ_ZSH_EDITOR_CLEAR_PHASE=2
    command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=editor-cleared\033\\' \
      "$MEZ_ZSH_HISTORY_TOKEN"
    return 0
  fi
  __MEZ_ZSH_EDITOR_CLEAR_PHASE=0
  BUFFER=__mez_zsh_private_receiver
  CURSOR=${#BUFFER}
  zle accept-line
}
function __mez_zsh_line_init() {
  emulate -L zsh
  if [[ -n ${__MEZ_ZSH_RESTORE_MARKER} ]]; then
    command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=parent-ready;mez_marker=%s;mez_outcome=%s;mez_status=%s\033\\' \
      "$MEZ_ZSH_HISTORY_TOKEN" "$__MEZ_ZSH_RESTORE_MARKER" "$__MEZ_ZSH_RESTORE_OUTCOME" "$__MEZ_ZSH_RESTORE_STATUS"
    __MEZ_ZSH_RESTORE_MARKER=
  fi
  if [[ -n ${MEZ_ZSH_RECEIVER_INSTALL_MARKER-} ]]; then
    command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=child-installed;mez_marker=%s\033\\' \
      "$MEZ_ZSH_HISTORY_TOKEN" "$MEZ_ZSH_RECEIVER_INSTALL_MARKER"
    unset MEZ_ZSH_RECEIVER_INSTALL_MARKER
  fi
}
function __mez_zsh_binding_available() {
  emulate -L zsh
  local keymap=$1 sequence=$2 binding
  binding=$(bindkey -M ${keymap} ${sequence} 2>/dev/null) || return 1
  [[ ${binding} == *' undefined-key' || ${binding} == *' __mez_zsh_private_widget' ]]
}
function __mez_zsh_install_integration() {
  emulate -L zsh
  local keymap
  local -a mez_keymaps=(emacs viins vicmd)
  autoload -Uz add-zsh-hook 2>/dev/null || true
  add-zsh-hook -d precmd __mez_zsh_install_integration 2>/dev/null || true
  __MEZ_ZSH_ADMISSION_READY=0
  __MEZ_ZSH_TRIGGER_ID=
  __MEZ_ZSH_TRIGGER_SEQUENCE=
  for keymap in ${mez_keymaps}; do
    __mez_zsh_binding_available ${keymap} $'\e[27;9;109~' || break
  done
  if [[ ${keymap-} == vicmd ]] && __mez_zsh_binding_available vicmd $'\e[27;9;109~'; then
    __MEZ_ZSH_TRIGGER_ID=escape-m
    __MEZ_ZSH_TRIGGER_SEQUENCE=$'\e[27;9;109~'
  else
    for keymap in ${mez_keymaps}; do
      __mez_zsh_binding_available ${keymap} $'\e[27;9;110~' || break
    done
    if [[ ${keymap-} == vicmd ]] && __mez_zsh_binding_available vicmd $'\e[27;9;110~'; then
      __MEZ_ZSH_TRIGGER_ID=escape-n
      __MEZ_ZSH_TRIGGER_SEQUENCE=$'\e[27;9;110~'
    fi
  fi
  if [[ -z ${__MEZ_ZSH_TRIGGER_ID} ]]; then
    command printf '\033]133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=%s;mez_reason=no-free-trigger\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    return 1
  fi
  autoload -Uz add-zle-hook-widget 2>/dev/null || {
    command printf '\033]133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=%s;mez_reason=line-init-hook-unavailable\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    return 1
  }
  zle -N __mez_zsh_private_widget || return 1
  add-zle-hook-widget line-init __mez_zsh_line_init 2>/dev/null || {
    zle -D __mez_zsh_private_widget 2>/dev/null || true
    command printf '\033]133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=%s;mez_reason=line-init-hook-failed\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    return 1
  }
  local bound=1
  for keymap in ${mez_keymaps}; do
    bindkey -M ${keymap} ${__MEZ_ZSH_TRIGGER_SEQUENCE} __mez_zsh_private_widget 2>/dev/null || bound=0
  done
  if (( ! bound )); then
    for keymap in ${mez_keymaps}; do
      bindkey -rM ${keymap} ${__MEZ_ZSH_TRIGGER_SEQUENCE} 2>/dev/null || true
    done
    add-zle-hook-widget -d line-init __mez_zsh_line_init 2>/dev/null || true
    zle -D __mez_zsh_private_widget 2>/dev/null || true
    command printf '\033]133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=%s;mez_reason=trigger-bind-failed\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    return 1
  fi
  __MEZ_ZSH_ADMISSION_READY=1
  command printf '\033]133;R;mez_receiver=available;mez_shell=zsh;mez_token=%s;mez_trigger=%s\033\\' \
    "$MEZ_ZSH_HISTORY_TOKEN" "$__MEZ_ZSH_TRIGGER_ID"
  command printf '\033]133;R;mez_protocol=2;mez_shell=zsh;mez_token=%s;mez_event=adapter-available\033\\' \
    "$MEZ_ZSH_HISTORY_TOKEN"
}
function __mez_zsh_schedule_integration() {
  emulate -L zsh
  autoload -Uz add-zsh-hook 2>/dev/null || {
    command printf '\033]133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=%s;mez_reason=precmd-hook-unavailable\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    return 1
  }
  add-zsh-hook -d precmd __mez_zsh_install_integration 2>/dev/null || true
  add-zsh-hook precmd __mez_zsh_install_integration 2>/dev/null || {
    command printf '\033]133;R;mez_receiver=unavailable;mez_shell=zsh;mez_token=%s;mez_reason=precmd-hook-failed\033\\' "$MEZ_ZSH_HISTORY_TOKEN"
    return 1
  }
}
if [[ -o interactive ]]; then
  __mez_zsh_schedule_integration
fi
if [[ ${MEZ_ZSH_INTEGRATION_ONLY:-0} != 1 ]]; then
  ZDOTDIR=${MEZ_ZSH_USER_ZDOTDIR}
  unset MEZ_ZSH_MANAGED_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR MEZ_ZSH_ORIGINAL_ZDOTDIR_WAS_SET
fi
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
        let managed_zlogin = MANAGED_ZLOGIN
            .replace(
                "__MEZ_ZSH_MAX_SOURCE_BYTES__",
                &mez_agent::ZSH_PRIVATE_SOURCE_MAX_BYTES.to_string(),
            )
            .replace(
                "__MEZ_ZSH_MAX_SOURCE_CHUNKS__",
                &mez_agent::ZSH_PRIVATE_SOURCE_MAX_CHUNKS.to_string(),
            )
            .replace(
                "__MEZ_ZSH_MAX_RECORD_BYTES__",
                &mez_agent::ZSH_PRIVATE_SOURCE_MAX_RECORD_BYTES.to_string(),
            )
            .replace(
                "__MEZ_ZSH_MAX_DATA_BYTES__",
                &mez_agent::ZSH_PRIVATE_SOURCE_DATA_MAX_BYTES.to_string(),
            );
        if let Err(error) = write_private_file(&directory.join(".zshenv"), MANAGED_ZSHENV)
            .and_then(|()| write_private_file(&directory.join(".zprofile"), MANAGED_ZPROFILE))
            .and_then(|()| write_private_file(&directory.join(".zshrc"), MANAGED_ZSHRC))
            .and_then(|()| write_private_file(&directory.join(".zlogin"), &managed_zlogin))
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

    /// Returns immutable startup state for a managed login-interactive child.
    pub(super) fn shell_descriptor(
        &self,
        trigger: mez_agent::ManagedZshTrigger,
    ) -> Result<mez_agent::ManagedZshShell> {
        mez_agent::ManagedZshShell::new(self.token.clone(), self.directory.clone(), trigger)
            .map_err(|error| MezError::invalid_state(error.to_string()))
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
    use mez_mux::layout::Size;
    use mez_mux::process::{
        PaneProcess, PaneProcessEnvironment, pane_command_plan, spawn_pane_process,
    };
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Builds the dependency-neutral pane environment for one Zsh PTY test.
    fn test_environment() -> PaneProcessEnvironment {
        PaneProcessEnvironment {
            mez: "socket=/tmp/mez-test.sock;session=s1;window=w1;pane=p1;term=xterm-256color"
                .to_string(),
            session: "s1".to_string(),
            window: "w1".to_string(),
            pane: "p1".to_string(),
            term: "xterm-256color".to_string(),
        }
    }

    /// Drives one managed Zsh PTY through terminal query responses.
    struct ManagedZshTestPane {
        process: PaneProcess,
        terminal: mez_terminal::TerminalScreen,
    }

    impl ManagedZshTestPane {
        /// Reads output and returns terminal-generated responses to Zsh.
        fn read_available_output(&mut self, max_bytes: usize) -> mez_mux::Result<Vec<u8>> {
            let output = self.process.read_available_output(max_bytes)?;
            self.terminal.feed(&output);
            let responses = self.terminal.drain_terminal_response_bytes();
            if !responses.is_empty() {
                self.process.write_input(&responses)?;
            }
            Ok(output)
        }

        /// Writes user or managed input to the Zsh PTY.
        fn write_input(&mut self, input: &[u8]) -> mez_mux::Result<()> {
            self.process.write_input(input)
        }

        /// Terminates the managed Zsh process within the supplied deadline.
        fn terminate(
            &mut self,
            timeout: Duration,
        ) -> mez_mux::Result<mez_mux::process::PaneExitStatus> {
            self.process.terminate(timeout)
        }
    }

    /// Reads managed Zsh output until one expected boundary appears.
    fn extend_zsh_output_until(
        process: &mut ManagedZshTestPane,
        output: &mut Vec<u8>,
        predicate: impl Fn(&[u8]) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            output.extend(
                process
                    .read_available_output(64 * 1024)
                    .expect("managed Zsh output should remain readable"),
            );
            if predicate(output) {
                return;
            }
            if Instant::now() >= deadline {
                let _ = process.terminate(Duration::from_millis(100));
                panic!(
                    "managed Zsh output did not reach its expected boundary: {:?}",
                    String::from_utf8_lossy(output)
                );
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Reads one managed Zsh PTY until one expected boundary appears.
    fn read_zsh_output_until(
        process: &mut ManagedZshTestPane,
        predicate: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        let mut output = Vec::new();
        extend_zsh_output_until(process, &mut output, predicate);
        output
    }

    /// Drives the fixed ZLE trigger and waits for authenticated editor ownership.
    fn hold_managed_zsh_editor(
        process: &mut ManagedZshTestPane,
        admission: &mez_agent::ZshPrivateSourceInput,
        owner: &MarkerToken,
        marker: &str,
    ) -> Vec<u8> {
        process.write_input(admission.wrapper.as_bytes()).unwrap();
        let clear_requested = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=editor-clear-requested",
            owner.as_str()
        );
        let mut output = read_zsh_output_until(process, |output| {
            output
                .windows(clear_requested.len())
                .any(|window| window == clear_requested.as_bytes())
        });
        process.write_input(admission.wrapper.as_bytes()).unwrap();
        let editor_cleared = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=editor-cleared",
            owner.as_str()
        );
        extend_zsh_output_until(process, &mut output, |output| {
            output
                .windows(editor_cleared.len())
                .any(|window| window == editor_cleared.as_bytes())
        });
        process.write_input(admission.wrapper.as_bytes()).unwrap();
        let awaiting = format!("mez_receiver=awaiting;mez_token={}", owner.as_str());
        extend_zsh_output_until(process, &mut output, |output| {
            output
                .windows(awaiting.len())
                .any(|window| window == awaiting.as_bytes())
        });
        process
            .write_input(admission.receiver_hold.as_bytes())
            .unwrap();
        let editor_held = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=editor-held;mez_marker={marker}",
            owner.as_str()
        );
        extend_zsh_output_until(process, &mut output, |output| {
            output
                .windows(editor_held.len())
                .any(|window| window == editor_held.as_bytes())
        });
        output
    }

    /// Releases Zsh BEGIN after editor hold and waits for frame admission.
    fn admit_managed_zsh_frame(
        process: &mut ManagedZshTestPane,
        admission: &mez_agent::ZshPrivateSourceInput,
        owner: &MarkerToken,
        marker: &str,
    ) -> Vec<u8> {
        let mut output = hold_managed_zsh_editor(process, admission, owner, marker);
        process
            .write_input(admission.receiver_admission.as_bytes())
            .unwrap();
        let frame_admitted = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=frame-admitted;mez_marker={marker}",
            owner.as_str()
        );
        extend_zsh_output_until(process, &mut output, |output| {
            output
                .windows(frame_admitted.len())
                .any(|window| window == frame_admitted.as_bytes())
        });
        output
    }

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
    /// install through the real login-interactive managed handoff, retain
    /// ordinary commands, and return control to the original parent while
    /// neither authenticated transport nor generated source reaches history.
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
        let launch = compatibility
            .configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()))
            .with_environment_variable("HOME", home.as_os_str());
        let size = Size::new(80, 24).unwrap();
        let process = spawn_pane_process(&launch, None, &test_environment(), size).unwrap();
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000).unwrap();
        let mut process = ManagedZshTestPane { process, terminal };
        let parent_pid = process.process.primary_pid();
        let parent_process_group = process.process.process_group_leader();
        let available = format!(
            "mez_receiver=available;mez_shell=zsh;mez_token={};mez_trigger=escape-m",
            token.as_str()
        );
        let _ = read_zsh_output_until(&mut process, |output| {
            output
                .windows(available.len())
                .any(|window| window == available.as_bytes())
        });

        let bootstrap_marker = "zsh-real-child-bootstrap";
        let exit_marker = MarkerToken::new("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee").unwrap();
        let descriptor = compatibility
            .shell_descriptor(mez_agent::ManagedZshTrigger::EscapeM)
            .unwrap();
        let handoff = agent_subshell_enter_command_with_shell_compatibility_and_exit_marker(
            zsh,
            ShellClassification::Zsh,
            Some(&token),
            Some(&descriptor),
            None,
            None,
            None,
            Some(bootstrap_marker),
            Some(&exit_marker),
        )
        .unwrap();
        let admission = mez_agent::zsh_private_source_input(
            &handoff,
            &token,
            bootstrap_marker,
            mez_agent::ManagedZshTrigger::EscapeM,
        )
        .unwrap();
        let mut output =
            admit_managed_zsh_frame(&mut process, &admission, &token, bootstrap_marker);
        process
            .write_input(admission.receiver_payload.as_bytes())
            .unwrap();
        let installed = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=child-installed;mez_marker={bootstrap_marker}",
            token.as_str()
        );
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(installed.len())
                .any(|window| window == installed.as_bytes())
        });

        process
            .write_input(b"print -r -- USER_CHILD\nprint -r -- __HISTORY_BEGIN__\nfc -l -100\nprint -r -- __HISTORY_END__\nexit\n")
            .unwrap();
        let restored = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=parent-ready;mez_marker={bootstrap_marker};mez_outcome=completed;mez_status=0",
            token.as_str()
        );
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        assert_eq!(process.process.primary_pid(), parent_pid);
        assert_eq!(process.process.process_group_leader(), parent_process_group);

        process.terminate(Duration::from_millis(100)).unwrap();
        let rendered = String::from_utf8_lossy(&output);
        let persisted = fs::read_to_string(&history).unwrap();
        let in_memory = rendered
            .split_once("__HISTORY_BEGIN__\r\n")
            .and_then(|(_, tail)| tail.split_once("__HISTORY_END__"))
            .map(|(history, _)| history)
            .unwrap_or_default();
        for observed in [&persisted, in_memory] {
            assert!(observed.contains("USER_CHILD"), "{observed}");
            assert!(!observed.contains("MEZ_ZSH_RX1"), "{observed}");
            assert!(!observed.contains(token.as_str()), "{observed}");
            assert!(!observed.contains("fc -p"), "{observed}");
        }

        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies private Zsh admission executes managed source while discarding
    /// a multiline Unicode editor draft and returning a responsive empty ZLE
    /// buffer without executing or recording the discarded command.
    ///
    /// A fresh command must execute immediately after return so this test
    /// proves parent responsiveness rather than only visual clearing.
    #[test]
    fn managed_zsh_private_admission_discards_dirty_draft() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "mez-managed-zsh-private-draft-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
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
                "PS1='__MEZ_ZSH_PROMPT__>'\nRPS1=\nbindkey -e\nHISTFILE={}\nHISTSIZE=100\nSAVEHIST=100\nsetopt INC_APPEND_HISTORY\nfunction __mez_test_user_line_init() {{ print -r -- line-init >> \"$HOME/line-init.log\" }}\nzle -N zle-line-init __mez_test_user_line_init\n",
                shell_single_quote_path(&history),
            ),
        )
        .unwrap();

        let owner = MarkerToken::new("99999999999999999999999999999999").unwrap();
        let compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            owner.clone(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let launch = compatibility
            .configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()))
            .with_environment_variable("HOME", home.as_os_str());
        let size = Size::new(80, 24).unwrap();
        let process = spawn_pane_process(&launch, None, &test_environment(), size).unwrap();
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000).unwrap();
        let mut process = ManagedZshTestPane { process, terminal };
        let parent_pid = process.process.primary_pid();
        let parent_process_group = process.process.process_group_leader();
        let startup_output = read_zsh_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_ZSH_PROMPT__>".len())
                .any(|window| window == b"__MEZ_ZSH_PROMPT__>")
        });
        let available = format!(
            "mez_receiver=available;mez_shell=zsh;mez_token={};mez_trigger=escape-m",
            owner.as_str()
        );
        assert!(
            startup_output
                .windows(available.len())
                .any(|window| window == available.as_bytes()),
            "managed zsh did not publish the expected trigger: {:?}",
            String::from_utf8_lossy(&startup_output)
        );

        let draft = "if true; then\n  print -r -- '__MEZ_ZSH_DRAFT_αβ_EXECUTED__'\nfi";
        let beta_byte = draft.find('β').unwrap();
        let cursor_left = draft.chars().count() - draft[..beta_byte].chars().count();
        let mut draft_input = b"\x1b[200~".to_vec();
        draft_input.extend_from_slice(draft.as_bytes());
        draft_input.extend_from_slice(b"\x1b[201~");
        draft_input.extend(std::iter::repeat_n(b'\x02', cursor_left));
        process.write_input(&draft_input).unwrap();
        let marker = "zsh-private-draft-marker";
        let admission = mez_agent::zsh_private_source_input(
            "print -r -- '__MEZ_ZSH_SOURCE_EXECUTED__'\n",
            &owner,
            marker,
            mez_agent::ManagedZshTrigger::EscapeM,
        )
        .unwrap();
        let mut output = admit_managed_zsh_frame(&mut process, &admission, &owner, marker);
        assert!(!String::from_utf8_lossy(&output).contains("__MEZ_ZSH_DRAFT_αβ_EXECUTED__\r\n"));

        process
            .write_input(admission.receiver_payload.as_bytes())
            .unwrap();
        let restored = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=completed;mez_status=0",
            owner.as_str()
        );
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        assert!(
            String::from_utf8_lossy(&output).contains("__MEZ_ZSH_SOURCE_EXECUTED__"),
            "{:?}",
            String::from_utf8_lossy(&output)
        );
        assert!(!String::from_utf8_lossy(&output).contains("__MEZ_ZSH_DRAFT_αβ_EXECUTED__\r\n"));

        process
            .write_input(b"print -r -- '__MEZ_ZSH_PARENT_RESPONSIVE__'\n")
            .unwrap();
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_ZSH_PARENT_RESPONSIVE__\r\n".len())
                .any(|window| window == b"__MEZ_ZSH_PARENT_RESPONSIVE__\r\n")
        });
        assert_eq!(process.process.primary_pid(), parent_pid);
        assert_eq!(process.process.process_group_leader(), parent_process_group);
        assert!(
            fs::read_to_string(home.join("line-init.log"))
                .unwrap()
                .lines()
                .count()
                >= 2,
            "the user zle-line-init widget should run before and after admission"
        );

        process.terminate(Duration::from_millis(100)).unwrap();
        let persisted_history = fs::read_to_string(&history).unwrap();
        assert!(!persisted_history.contains("__MEZ_ZSH_DRAFT_αβ_EXECUTED__"));
        assert!(
            !persisted_history.contains("__mez_zsh_private_receiver"),
            "{persisted_history}"
        );
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies malformed private frames discard the pending draft, while a
    /// continuation prompt rejects admission without executing managed input.
    #[test]
    fn managed_zsh_private_admission_fails_closed_for_malformed_and_continuation_state() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "mez-managed-zsh-private-failure-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let user_zdotdir = root.join("user-zdotdir");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_zdotdir).unwrap();
        fs::write(
            user_zdotdir.join(".zshrc"),
            "PS1='__MEZ_ZSH_PROMPT__>'\nPS2='__MEZ_ZSH_CONTINUATION__>'\nRPS1=\n",
        )
        .unwrap();

        let owner = MarkerToken::new("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            owner.clone(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let launch = compatibility
            .configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()))
            .with_environment_variable("HOME", home.as_os_str());
        let size = Size::new(80, 24).unwrap();
        let process = spawn_pane_process(&launch, None, &test_environment(), size).unwrap();
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000).unwrap();
        let mut process = ManagedZshTestPane { process, terminal };
        let _ = read_zsh_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_ZSH_PROMPT__>".len())
                .any(|window| window == b"__MEZ_ZSH_PROMPT__>")
        });

        let discarded_path = root.join("discarded-malformed-draft");
        process
            .write_input(
                format!(
                    "command touch {}",
                    mez_agent::shell::shell_quote(&discarded_path.to_string_lossy())
                )
                .as_bytes(),
            )
            .unwrap();
        let admission = mez_agent::zsh_private_source_input(
            "print -r -- SHOULD_NOT_RUN\n",
            &owner,
            "bad",
            mez_agent::ManagedZshTrigger::EscapeM,
        )
        .unwrap();
        let mut output = hold_managed_zsh_editor(&mut process, &admission, &owner, "bad");
        process
            .write_input(b"MEZ_ZSH_RX1_BEGIN wrong-token bad 1 0 1\n")
            .unwrap();
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_ZSH_PROMPT__>".len())
                .any(|window| window == b"__MEZ_ZSH_PROMPT__>")
        });
        process
            .write_input(b"print -r -- __MEZ_ZSH_AFTER_MALFORMED__\n")
            .unwrap();
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_ZSH_AFTER_MALFORMED__\r\n".len())
                .any(|window| window == b"__MEZ_ZSH_AFTER_MALFORMED__\r\n")
        });
        assert!(
            !discarded_path.exists(),
            "discarded malformed-frame draft executed"
        );
        assert!(!String::from_utf8_lossy(&output).contains("SHOULD_NOT_RUN"));

        process.write_input(b"print -r -- '\n").unwrap();
        let mut continuation = read_zsh_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_ZSH_CONTINUATION__>".len())
                .any(|window| window == b"__MEZ_ZSH_CONTINUATION__>")
        });
        process.write_input(admission.wrapper.as_bytes()).unwrap();
        thread::sleep(Duration::from_millis(50));
        continuation.extend(process.read_available_output(64 * 1024).unwrap());
        assert!(
            !String::from_utf8_lossy(&continuation).contains("mez_event=editor-held"),
            "{:?}",
            String::from_utf8_lossy(&continuation)
        );
        process.write_input(b"\x03").unwrap();

        process.terminate(Duration::from_millis(100)).unwrap();
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies user RCS opt-out remains native while managed admission uses an
    /// alternate trigger instead of replacing an existing binding.
    ///
    /// A user `.zshenv` may disable later startup files. Mezzanine must still
    /// publish authenticated admission, must not force RCS back on, must leave
    /// the skipped `.zshrc` untouched, and must preserve the occupied trigger
    /// in every supported keymap.
    #[test]
    fn managed_zsh_respects_no_rcs_and_preserves_existing_trigger_bindings() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "mez-managed-zsh-no-rcs-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let user_zdotdir = root.join("user-zdotdir");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_zdotdir).unwrap();
        fs::write(
            user_zdotdir.join(".zshenv"),
            "PS1='__MEZ_ZSH_NO_RCS__>'\nRPS1=\n\
bindkey -M emacs '^[[27;9;109~' self-insert\n\
bindkey -M viins '^[[27;9;109~' self-insert\n\
bindkey -M vicmd '^[[27;9;109~' self-insert\n\
unsetopt RCS\n",
        )
        .unwrap();
        fs::write(
            user_zdotdir.join(".zshrc"),
            "print -r -- SHOULD_NOT_SOURCE_ZSHRC\n",
        )
        .unwrap();

        let owner = MarkerToken::new("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb").unwrap();
        let compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            owner.clone(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let launch = compatibility
            .configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()))
            .with_environment_variable("HOME", home.as_os_str());
        let size = Size::new(80, 24).unwrap();
        let process = spawn_pane_process(&launch, None, &test_environment(), size).unwrap();
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000).unwrap();
        let mut process = ManagedZshTestPane { process, terminal };
        let expected = format!(
            "mez_receiver=available;mez_shell=zsh;mez_token={};mez_trigger=escape-n",
            owner.as_str()
        );
        let output = read_zsh_output_until(&mut process, |output| {
            output
                .windows(expected.len())
                .any(|window| window == expected.as_bytes())
        });

        assert!(!String::from_utf8_lossy(&output).contains("SHOULD_NOT_SOURCE_ZSHRC"));
        process
            .write_input(
                b"print -r -- RCS_STATE:${options[RCS]}\nfor map in emacs viins vicmd; do bindkey -M $map '^[[27;9;109~'; done\n",
            )
            .unwrap();
        let mut output = output;
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"RCS_STATE:off".len())
                .any(|window| window == b"RCS_STATE:off")
                && output
                    .windows(b"self-insert".len())
                    .filter(|window| *window == b"self-insert")
                    .count()
                    >= 3
        });

        process.terminate(Duration::from_millis(100)).unwrap();
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies authenticated pre-BEGIN cancellation keeps the parent draft
    /// discarded without evaluating or launching the retained source.
    ///
    /// This is the early-hide boundary used by runtime after ZLE accepts the
    /// private trigger but before the deferred handoff frame is released.
    #[test]
    fn managed_zsh_private_admission_cancellation_discards_parent_draft() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "mez-managed-zsh-cancel-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let user_zdotdir = root.join("user-zdotdir");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_zdotdir).unwrap();
        fs::write(
            user_zdotdir.join(".zshrc"),
            "PS1='__MEZ_ZSH_CANCEL__>'\nRPS1=\n",
        )
        .unwrap();

        let owner = MarkerToken::new("cccccccccccccccccccccccccccccccc").unwrap();
        let compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            owner.clone(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let launch = compatibility
            .configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()))
            .with_environment_variable("HOME", home.as_os_str());
        let size = Size::new(80, 24).unwrap();
        let process = spawn_pane_process(&launch, None, &test_environment(), size).unwrap();
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000).unwrap();
        let mut process = ManagedZshTestPane { process, terminal };
        let _ = read_zsh_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_ZSH_CANCEL__>".len())
                .any(|window| window == b"__MEZ_ZSH_CANCEL__>")
        });

        process
            .write_input(b"print -r -- __MEZ_ZSH_CANCELLED_DRAFT__")
            .unwrap();
        let marker = "zsh-cancel-marker";
        let admission = mez_agent::zsh_private_source_input(
            "print -r -- SHOULD_NOT_RUN_AFTER_CANCEL\n",
            &owner,
            marker,
            mez_agent::ManagedZshTrigger::EscapeM,
        )
        .unwrap();
        let mut output = admit_managed_zsh_frame(&mut process, &admission, &owner, marker);
        let cancellation = mez_agent::zsh_private_source_cancel_input(&owner, marker);
        process.write_input(cancellation.as_bytes()).unwrap();
        let restored = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=cancelled;mez_status=130",
            owner.as_str()
        );
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        assert!(!String::from_utf8_lossy(&output).contains("SHOULD_NOT_RUN_AFTER_CANCEL"));

        process.write_input(b"\n").unwrap();
        process
            .write_input(b"print -r -- __MEZ_ZSH_AFTER_CANCEL__\n")
            .unwrap();
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_ZSH_AFTER_CANCEL__\r\n".len())
                .any(|window| window == b"__MEZ_ZSH_AFTER_CANCEL__\r\n")
        });
        assert!(!String::from_utf8_lossy(&output).contains("__MEZ_ZSH_CANCELLED_DRAFT__\r\n"));

        process.terminate(Duration::from_millis(100)).unwrap();
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Verifies an admitted malformed frame drains every declared DATA record
    /// and END before ordinary editor input resumes.
    ///
    /// Acknowledgement-paced remainder must never leak into the restored ZLE
    /// buffer after the first malformed DATA record causes evaluation to fail.
    #[test]
    fn managed_zsh_private_receiver_drains_malformed_admitted_frame() {
        let zsh = Path::new("/bin/zsh");
        if !zsh.exists() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "mez-managed-zsh-drain-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let user_zdotdir = root.join("user-zdotdir");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&user_zdotdir).unwrap();
        fs::write(
            user_zdotdir.join(".zshrc"),
            "PS1='__MEZ_ZSH_DRAIN__>'\nRPS1=\n",
        )
        .unwrap();

        let owner = MarkerToken::new("dddddddddddddddddddddddddddddddd").unwrap();
        let compatibility = ManagedZshCompatibility::create(
            &root.join("control.sock"),
            "%1",
            owner.clone(),
            Some(user_zdotdir.as_os_str().to_os_string()),
        )
        .unwrap();
        let launch = compatibility
            .configure_launch(PaneProcessLaunch::new(zsh.to_path_buf()))
            .with_environment_variable("HOME", home.as_os_str());
        let size = Size::new(80, 24).unwrap();
        let process = spawn_pane_process(&launch, None, &test_environment(), size).unwrap();
        let terminal = mez_terminal::TerminalScreen::new(size, 1_000).unwrap();
        let mut process = ManagedZshTestPane { process, terminal };
        let _ = read_zsh_output_until(&mut process, |output| {
            output
                .windows(b"__MEZ_ZSH_DRAIN__>".len())
                .any(|window| window == b"__MEZ_ZSH_DRAIN__>")
        });

        let marker = "zsh-drain-marker";
        let admission = mez_agent::zsh_private_source_input(
            "print -r -- SHOULD_NOT_RUN_MALFORMED\n",
            &owner,
            marker,
            mez_agent::ManagedZshTrigger::EscapeM,
        )
        .unwrap();
        let mut output = hold_managed_zsh_editor(&mut process, &admission, &owner, marker);
        let digest = "0".repeat(64);
        let frame = format!(
            "MEZ_ZSH_RX1_BEGIN {} {marker} 1 {digest} 2\n\
MEZ_ZSH_RX1_DATA {} {marker} 0 !\n\
MEZ_ZSH_RX1_DATA {} {marker} 1 QQ==\n\
MEZ_ZSH_RX1_END {} {marker} 2 1 {digest}\n",
            owner.as_str(),
            owner.as_str(),
            owner.as_str(),
            owner.as_str(),
        );
        process.write_input(frame.as_bytes()).unwrap();
        let restored = format!(
            "mez_protocol=2;mez_shell=zsh;mez_token={};mez_event=parent-ready;mez_marker={marker};mez_outcome=frame-rejected;mez_status=65",
            owner.as_str()
        );
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(restored.len())
                .any(|window| window == restored.as_bytes())
        });
        assert!(!String::from_utf8_lossy(&output).contains("SHOULD_NOT_RUN_MALFORMED"));

        process
            .write_input(b"print -r -- __MEZ_ZSH_AFTER_DRAIN__\n")
            .unwrap();
        extend_zsh_output_until(&mut process, &mut output, |output| {
            output
                .windows(b"__MEZ_ZSH_AFTER_DRAIN__\r\n".len())
                .any(|window| window == b"__MEZ_ZSH_AFTER_DRAIN__\r\n")
        });

        process.terminate(Duration::from_millis(100)).unwrap();
        drop(compatibility);
        fs::remove_dir_all(root).unwrap();
    }

    /// Quotes one test path as a literal zsh word.
    fn shell_single_quote_path(path: &Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
    }
}
