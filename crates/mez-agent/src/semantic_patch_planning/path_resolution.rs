//! Pane-side canonical path resolution for semantic patch transactions.
//!
//! Semantic patches can target a remote pane whose filesystem and operating
//! system differ from the Mezzanine process. This module therefore renders a
//! shell resolver rather than consulting local Rust filesystem APIs. GNU
//! `realpath -m` remains the preferred discovered implementation; panes that
//! lack it use a native GNU/BSD `readlink` component walker which resolves
//! existing symlinks while allowing missing create-target components.
//!
//! Both transaction phases embed the same resolver. The write phase can thus
//! compare a fresh canonical path with the read-phase snapshot before every
//! mutation. The resolver deliberately fails on unreadable links and bounded
//! symlink expansion rather than weakening filesystem boundary checks.

/// Maximum symbolic-link expansions accepted by the native shell resolver.
///
/// This matches the conventional Unix traversal bound and guarantees that a
/// cyclic link graph terminates even though the shell does not expose
/// `ELOOP` while inspecting one link at a time.
const MAX_SYMLINK_EXPANSIONS: usize = 40;

/// Renders the shared pane-shell canonical path resolver.
///
/// The emitted `mez_apply_patch_resolve` function accepts one relative or
/// absolute path and prints its absolute physical spelling. It returns nonzero
/// when GNU canonicalization and the native GNU/BSD fallback are both
/// unavailable, or when a symlink cannot be read or exceeds the expansion
/// bound.
pub(super) fn apply_patch_path_resolution_lines() -> Vec<String> {
    vec![
        "MEZ_APPLY_USE_REALPATH_M=".to_string(),
        "MEZ_APPLY_READLINK=".to_string(),
        "if command -v realpath >/dev/null 2>&1 && realpath -m -- / >/dev/null 2>&1; then"
            .to_string(),
        "  MEZ_APPLY_USE_REALPATH_M=1".to_string(),
        "elif [ -x /usr/bin/readlink ]; then".to_string(),
        "  MEZ_APPLY_READLINK=/usr/bin/readlink".to_string(),
        "elif [ -x /bin/readlink ]; then".to_string(),
        "  MEZ_APPLY_READLINK=/bin/readlink".to_string(),
        "else".to_string(),
        "  printf '%s\\n' 'apply_patch: GNU realpath -m or native readlink is required for apply_patch actions' >&2"
            .to_string(),
        "  exit 127".to_string(),
        "fi".to_string(),
        "mez_apply_patch_resolve() {".to_string(),
        "  if [ -n \"$MEZ_APPLY_USE_REALPATH_M\" ]; then realpath -m -- \"$1\"; return; fi"
            .to_string(),
        "  MEZ_APPLY_RESOLVE_QUEUE=$1".to_string(),
        "  case \"$MEZ_APPLY_RESOLVE_QUEUE\" in".to_string(),
        "    /*) MEZ_APPLY_RESOLVE_CURRENT=/; MEZ_APPLY_RESOLVE_QUEUE=${MEZ_APPLY_RESOLVE_QUEUE#/} ;;"
            .to_string(),
        "    *) MEZ_APPLY_RESOLVE_CURRENT=$MEZ_APPLY_CWD ;;".to_string(),
        "  esac".to_string(),
        "  MEZ_APPLY_RESOLVE_LINKS=0".to_string(),
        "  while [ -n \"$MEZ_APPLY_RESOLVE_QUEUE\" ]; do".to_string(),
        "    case \"$MEZ_APPLY_RESOLVE_QUEUE\" in".to_string(),
        "      */*) MEZ_APPLY_RESOLVE_COMPONENT=${MEZ_APPLY_RESOLVE_QUEUE%%/*}; MEZ_APPLY_RESOLVE_QUEUE=${MEZ_APPLY_RESOLVE_QUEUE#*/} ;;"
            .to_string(),
        "      *) MEZ_APPLY_RESOLVE_COMPONENT=$MEZ_APPLY_RESOLVE_QUEUE; MEZ_APPLY_RESOLVE_QUEUE= ;;"
            .to_string(),
        "    esac".to_string(),
        "    case \"$MEZ_APPLY_RESOLVE_COMPONENT\" in".to_string(),
        "      ''|.) continue ;;".to_string(),
        "      ..)".to_string(),
        "        if [ \"$MEZ_APPLY_RESOLVE_CURRENT\" != / ]; then MEZ_APPLY_RESOLVE_CURRENT=${MEZ_APPLY_RESOLVE_CURRENT%/*}; [ -n \"$MEZ_APPLY_RESOLVE_CURRENT\" ] || MEZ_APPLY_RESOLVE_CURRENT=/; fi"
            .to_string(),
        "        continue".to_string(),
        "        ;;".to_string(),
        "    esac".to_string(),
        "    if [ \"$MEZ_APPLY_RESOLVE_CURRENT\" = / ]; then MEZ_APPLY_RESOLVE_CANDIDATE=/$MEZ_APPLY_RESOLVE_COMPONENT; else MEZ_APPLY_RESOLVE_CANDIDATE=$MEZ_APPLY_RESOLVE_CURRENT/$MEZ_APPLY_RESOLVE_COMPONENT; fi"
            .to_string(),
        "    if [ -L \"$MEZ_APPLY_RESOLVE_CANDIDATE\" ]; then".to_string(),
        format!(
            "      MEZ_APPLY_RESOLVE_LINKS=$((MEZ_APPLY_RESOLVE_LINKS + 1)); if [ \"$MEZ_APPLY_RESOLVE_LINKS\" -gt {MAX_SYMLINK_EXPANSIONS} ]; then return 1; fi"
        ),
        "      MEZ_APPLY_RESOLVE_TARGET=$(\"$MEZ_APPLY_READLINK\" -n \"$MEZ_APPLY_RESOLVE_CANDIDATE\" && printf X) || return 1"
            .to_string(),
        "      MEZ_APPLY_RESOLVE_TARGET=${MEZ_APPLY_RESOLVE_TARGET%?}".to_string(),
        "      case \"$MEZ_APPLY_RESOLVE_TARGET\" in /*) MEZ_APPLY_RESOLVE_CURRENT=/; MEZ_APPLY_RESOLVE_TARGET=${MEZ_APPLY_RESOLVE_TARGET#/} ;; esac"
            .to_string(),
        "      if [ -n \"$MEZ_APPLY_RESOLVE_QUEUE\" ]; then MEZ_APPLY_RESOLVE_QUEUE=$MEZ_APPLY_RESOLVE_TARGET/$MEZ_APPLY_RESOLVE_QUEUE; else MEZ_APPLY_RESOLVE_QUEUE=$MEZ_APPLY_RESOLVE_TARGET; fi"
            .to_string(),
        "    else".to_string(),
        "      MEZ_APPLY_RESOLVE_CURRENT=$MEZ_APPLY_RESOLVE_CANDIDATE".to_string(),
        "    fi".to_string(),
        "  done".to_string(),
        "  printf '%s\\n' \"$MEZ_APPLY_RESOLVE_CURRENT\"".to_string(),
        "}".to_string(),
    ]
}
