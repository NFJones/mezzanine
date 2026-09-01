#!/bin/sh
# Run the Linux real-Xvfb X11 forwarding acceptance test without allowing
# missing X SECURITY support to downgrade untrusted forwarding to trusted mode.

set -eu

timeout_command="${TIMEOUT_COMMAND:-timeout}"
command -v "$timeout_command" >/dev/null 2>&1 || {
    echo "X11 forwarding validation requires $timeout_command" >&2
    exit 1
}
for tool in Xvfb xauth; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "X11 forwarding validation requires $tool" >&2
        exit 1
    }
done

root="$(mktemp -d "${TMPDIR:-/tmp}/mez-x11-validation.XXXXXX")"
display_number="${MEZ_X11_TEST_DISPLAY:-99}"
authority="$root/Xauthority"
log="$root/Xvfb.log"
xvfb_pid=""
cleanup() {
    if [ -n "$xvfb_pid" ]; then
        kill "$xvfb_pid" 2>/dev/null || true
        wait "$xvfb_pid" 2>/dev/null || true
    fi
    rm -rf "$root"
}
trap cleanup EXIT HUP INT TERM

umask 077
: > "$authority"
cookie="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
xauth -f "$authority" add ":$display_number" MIT-MAGIC-COOKIE-1 "$cookie"
# Keep the otherwise-idle test server from resetting after the short-lived
# `xauth generate` client disconnects and discarding its dynamic X SECURITY
# authorization before the forwarding round trip can use it.
Xvfb ":$display_number" -screen 0 800x600x24 -nolisten tcp -noreset -auth "$authority" >"$log" 2>&1 &
xvfb_pid=$!

attempt=0
while [ ! -S "/tmp/.X11-unix/X$display_number" ]; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 100 ] || ! kill -0 "$xvfb_pid" 2>/dev/null; then
        cat "$log" >&2
        echo "Xvfb did not become ready" >&2
        exit 1
    fi
    sleep 0.1
done

DISPLAY=":$display_number" \
XAUTHORITY="$authority" \
MEZ_X11_XVFB_TEST=1 \
"$timeout_command" 300s cargo test --quiet -p mezzanine --lib --all-features \
    cli::control_client::tests::x11_xvfb_trusted_and_untrusted_setup_round_trip \
    -- --exact --ignored --nocapture --test-threads=1
