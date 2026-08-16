#!/bin/sh
# Run the cross-platform managed-shell acceptance suite without permitting
# missing Bash, Fish, or Zsh binaries to turn real-PTY coverage into a skip.

set -eu

case "$(uname -s)" in
    Darwin)
        timeout_command="${TIMEOUT_COMMAND:-gtimeout}"
        ;;
    *)
        timeout_command="${TIMEOUT_COMMAND:-timeout}"
        ;;
esac

command -v "$timeout_command" >/dev/null 2>&1 || {
    echo "managed-shell reliability requires $timeout_command" >&2
    exit 1
}

require_shell() {
    shell_name="$1"
    shift
    for candidate in "$@"; do
        if [ -x "$candidate" ]; then
            "$candidate" --version | head -n 1
            return 0
        fi
    done
    echo "managed-shell reliability requires $shell_name at a supported path" >&2
    exit 1
}

require_shell Bash /bin/bash /usr/bin/bash
require_shell Fish /usr/bin/fish /usr/local/bin/fish /opt/homebrew/bin/fish
require_shell Zsh /bin/zsh /usr/bin/zsh /usr/local/bin/zsh

run_suite() {
    filter="$1"
    "$timeout_command" 300s cargo test --quiet -p mezzanine --lib --all-features \
        "$filter" -- --nocapture --test-threads=1
}

run_exact() {
    test_name="$1"
    "$timeout_command" 300s cargo test --quiet -p mezzanine --lib --all-features \
        "$test_name" -- --exact --nocapture --test-threads=1
}

run_suite runtime::processes::bash_compat::tests
run_suite runtime::processes::fish_compat::tests
run_suite runtime::processes::zsh_compat::tests
run_suite runtime::processes::managed_shell_handoff::tests
run_suite runtime::tests::actions::shell_protocol
run_exact host::async_runtime::tests::services::pane_service::async_fish_dirty_draft_no_prompt_exit_restores_responsive_parent
run_exact host::async_runtime::tests::services::pane_service::async_zsh_dirty_draft_no_prompt_exit_restores_responsive_parent
run_exact host::async_runtime::tests::services::pane_service::async_pane_process_service_aggregates_receiver_delivery_progress

if [ "$(uname -s)" = Darwin ]; then
    "$timeout_command" 600s cargo test --quiet -p mezzanine --lib --all-features \
        host::async_runtime::tests::services::semantic_patch::async_zsh_large_semantic_patch_completes_and_releases_input \
        -- --exact --nocapture --test-threads=1
fi
