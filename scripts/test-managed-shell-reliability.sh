#!/bin/sh
# Run the cross-platform managed-shell acceptance suite without permitting
# missing Bash, Fish, or Zsh binaries to turn real-PTY coverage into a skip.

set -eu

cargo_command="${CARGO_COMMAND:-cargo}"

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

command -v "$cargo_command" >/dev/null 2>&1 || {
    echo "managed-shell reliability requires $cargo_command" >&2
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

build_timeout="${MANAGED_SHELL_BUILD_TIMEOUT:-900s}"
suite_timeout="${MANAGED_SHELL_SUITE_TIMEOUT:-300s}"
long_suite_timeout="${MANAGED_SHELL_LONG_SUITE_TIMEOUT:-600s}"

build_test_binary() {
    package="$1"
    target_name="$2"
    artifact_file="$(mktemp "${TMPDIR:-/tmp}/mez-managed-shell-artifact.XXXXXX")"
    trap 'rm -f "$artifact_file"' EXIT HUP INT TERM

    printf '==> Build %s test binary (budget: %s)\n' "$package" "$build_timeout" >&2
    "$timeout_command" "$build_timeout" "$cargo_command" test --quiet -p "$package" --lib --all-features \
        --no-run --message-format=json >"$artifact_file"

    test_binary="$(python3 -c '
import json
import sys

target_name = sys.argv[1]
for line in sys.stdin:
    message = json.loads(line)
    if (message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == target_name
            and message.get("executable")):
        print(message["executable"])
        break
else:
    raise SystemExit(f"did not find test executable for {target_name}")
' "$target_name" <"$artifact_file")"
    rm -f "$artifact_file"
    trap - EXIT HUP INT TERM

    [ -n "$test_binary" ] || {
        echo "managed-shell reliability could not locate $package test binary" >&2
        exit 1
    }
    printf '%s\n' "$test_binary"
}

run_suite() {
    test_binary="$1"
    filter="$2"
    printf '==> Run %s (budget: %s)\n' "$filter" "$suite_timeout"
    "$timeout_command" "$suite_timeout" "$test_binary" "$filter" --nocapture --test-threads=1
}

run_exact() {
    test_binary="$1"
    test_name="$2"
    printf '==> Run %s (budget: %s)\n' "$test_name" "$suite_timeout"
    "$timeout_command" "$suite_timeout" "$test_binary" "$test_name" --exact --nocapture --test-threads=1
}

run_agent_exact() {
    test_binary="$1"
    test_name="$2"
    printf '==> Run %s (budget: %s)\n' "$test_name" "$suite_timeout"
    "$timeout_command" "$suite_timeout" "$test_binary" "$test_name" --exact --nocapture --test-threads=1
}

mezzanine_test_binary="$(build_test_binary mezzanine mezzanine)"
mez_agent_test_binary="$(build_test_binary mez-agent mez_agent)"

run_suite "$mezzanine_test_binary" runtime::processes::bash_compat::tests
run_suite "$mezzanine_test_binary" runtime::processes::fish_compat::tests
run_suite "$mezzanine_test_binary" runtime::processes::zsh_compat::tests
run_suite "$mezzanine_test_binary" runtime::processes::managed_shell_handoff::tests
run_suite "$mezzanine_test_binary" runtime::tests::actions::shell_protocol
run_agent_exact "$mez_agent_test_binary" shell::tests::shell_transport::managed_zsh_maximum_source_uses_bounded_acknowledgement_frames
run_exact "$mezzanine_test_binary" host::async_runtime::pane_io::delivery::tests::managed_zsh_physical_records_wait_only_at_logical_frame_boundaries
run_exact "$mezzanine_test_binary" host::async_runtime::tests::services::pane_service::async_fish_dirty_draft_no_prompt_exit_discards_draft_and_restores_responsive_parent
run_exact "$mezzanine_test_binary" host::async_runtime::tests::services::pane_service::async_zsh_dirty_draft_no_prompt_exit_discards_draft_and_restores_responsive_parent
run_exact "$mezzanine_test_binary" host::async_runtime::tests::services::pane_service::async_pane_process_service_aggregates_receiver_delivery_progress

if [ "$(uname -s)" = Darwin ]; then
    printf '==> Run %s (budget: %s)\n' \
        host::async_runtime::tests::services::semantic_patch::async_zsh_large_semantic_patch_completes_and_releases_input \
        "$long_suite_timeout"
    "$timeout_command" "$long_suite_timeout" "$mezzanine_test_binary" \
        host::async_runtime::tests::services::semantic_patch::async_zsh_large_semantic_patch_completes_and_releases_input \
        --exact --nocapture --test-threads=1
fi
