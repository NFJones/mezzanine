#!/bin/sh
# Qualify the production Linux power-inhibition backend only after proving the
# real host provides every required D-Bus service. This script performs no idle
# configuration writes and permits no fake-service or weaker-mode fallback.

set -eu

fail() {
    printf '%s\n' "real Linux power-inhibition qualification: $*" >&2
    exit 1
}

[ "${MEZ_REAL_LINUX_POWER_INHIBITION:-}" = "1" ] ||
    fail "set MEZ_REAL_LINUX_POWER_INHIBITION=1 to authorize the real-host test"
[ "$(uname -s 2>/dev/null || true)" = "Linux" ] ||
    fail "requires Linux"

kernel_evidence="$(cat /proc/sys/kernel/osrelease /proc/version 2>/dev/null || true)"
if [ -n "${WSL_INTEROP:-}" ] || [ -n "${WSL_DISTRO_NAME:-}" ] ||
    printf '%s\n' "$kernel_evidence" | grep -Eiq 'microsoft|wsl'; then
    fail "WSL is unsupported because a Linux guest cannot inhibit Windows host power"
fi

command -v busctl >/dev/null 2>&1 || fail "requires busctl for read-only D-Bus preflight"
command -v cargo >/dev/null 2>&1 || fail "requires cargo"
timeout_command="${TIMEOUT_COMMAND:-timeout}"
command -v "$timeout_command" >/dev/null 2>&1 || fail "requires $timeout_command"

[ -d /run/systemd/system ] || fail "requires a host booted with systemd"
[ -S /run/dbus/system_bus_socket ] || fail "requires the system D-Bus socket"
[ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || fail "requires DBUS_SESSION_BUS_ADDRESS"

system_owner="$({ busctl --system --no-pager call \
    org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus \
    NameHasOwner s org.freedesktop.login1; } 2>/dev/null)" ||
    fail "cannot query the system bus for systemd-logind"
[ "$system_owner" = "b true" ] || fail "org.freedesktop.login1 has no system-bus owner"

screensaver_owner="$({ busctl --user --no-pager call \
    org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus \
    NameHasOwner s org.freedesktop.ScreenSaver; } 2>/dev/null)" ||
    fail "cannot query the session bus for org.freedesktop.ScreenSaver"
[ "$screensaver_owner" = "b true" ] ||
    fail "org.freedesktop.ScreenSaver has no session-bus owner"

MEZ_REAL_LINUX_POWER_INHIBITION=1 \
    "$timeout_command" 120s cargo test --quiet -p mezzanine --lib --all-features \
    host::power_inhibition::linux::tests::production_backend_acquires_system_and_display_then_becomes_inactive \
    -- --exact --ignored --nocapture --test-threads=1
