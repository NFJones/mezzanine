#!/bin/sh
# Exercise the managed-shell CI wrapper with fake Cargo, timeout, and harness
# executables. This keeps the regression fast while proving that compilation
# is isolated from execution and that harness failures retain their exit code.

set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
fixture_root="$(mktemp -d "${TMPDIR:-/tmp}/mez-managed-shell-test.XXXXXX")"
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM
mkdir -p "$fixture_root/bin"

cat >"$fixture_root/bin/cargo" <<'EOF'
#!/bin/sh
set -eu
package=""
previous=""
for argument in "$@"; do
    if [ "$previous" = "-p" ]; then
        package="$argument"
    fi
    previous="$argument"
done
case "$package" in
    mezzanine) target=mezzanine ;;
    mez-agent) target=mez_agent ;;
    *) exit 91 ;;
esac
printf '{"reason":"compiler-artifact","target":{"name":"%s"},"executable":"%s/%s-test"}\n' \
    "$target" "$FAKE_ROOT" "$target"
EOF

cat >"$fixture_root/bin/timeout" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" >>"$FAKE_LOG"
shift
exec "$@"
EOF

cat >"$fixture_root/mezzanine-test" <<'EOF'
#!/bin/sh
set -eu
printf 'mezzanine %s\n' "$*" >>"$FAKE_LOG"
case "$*" in
    *"managed_shell_handoff"*) exit "${FAKE_HARNESS_STATUS:-0}" ;;
esac
EOF

cat >"$fixture_root/mez_agent-test" <<'EOF'
#!/bin/sh
set -eu
printf 'mez-agent %s\n' "$*" >>"$FAKE_LOG"
EOF

chmod +x "$fixture_root/bin/cargo" "$fixture_root/bin/timeout" \
    "$fixture_root/mezzanine-test" "$fixture_root/mez_agent-test"

run_script() {
    FAKE_ROOT="$fixture_root" FAKE_LOG="$fixture_root/invocations" \
        PATH="$fixture_root/bin:$PATH" CARGO_COMMAND=cargo TIMEOUT_COMMAND=timeout \
        sh "$root/scripts/test-managed-shell-reliability.sh"
}

output="$(run_script 2>&1)"
printf '%s\n' "$output" | grep -F '==> Build mezzanine test binary (budget: 900s)' >/dev/null
printf '%s\n' "$output" | grep -F '==> Run runtime::processes::bash_compat::tests (budget: 300s)' >/dev/null
grep -F '900s cargo test --quiet -p mezzanine --lib --all-features --no-run --message-format=json' "$fixture_root/invocations" >/dev/null
grep -F 'mezzanine runtime::processes::bash_compat::tests --nocapture --test-threads=1' "$fixture_root/invocations" >/dev/null
grep -F 'mez-agent shell::tests::shell_transport::managed_zsh_maximum_source_uses_bounded_acknowledgement_frames --exact --nocapture --test-threads=1' "$fixture_root/invocations" >/dev/null
grep -F 'mezzanine host::async_runtime::pane_io::delivery::tests::managed_zsh_physical_records_wait_only_at_logical_frame_boundaries --exact --nocapture --test-threads=1' "$fixture_root/invocations" >/dev/null

set +e
FAKE_HARNESS_STATUS=17 run_script >/dev/null
status=$?
set -e
[ "$status" -eq 17 ] || {
    echo "expected harness failure to exit 17, got $status" >&2
    exit 1
}
