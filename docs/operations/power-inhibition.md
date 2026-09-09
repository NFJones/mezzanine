# Power inhibition

## Purpose

Configure and qualify Mezzanine's optional host power-inhibition policy for
running agent turns without changing the host's idle timers or claiming to
override explicit power-management decisions.

## Configure the policy

`agents.active_turn_sleep_inhibition` is disabled by default and is restricted
to direct primary-user configuration:

```toml
[agents]
active_turn_sleep_inhibition = "system-and-display"
```

The accepted values are:

- `disabled`: acquire no host power inhibitor.
- `system`: best-effort prevention of automatic idle system sleep.
- `system-and-display`: request the system inhibitor and display wakefulness.

Mezzanine derives the request from canonical agent turns in `Running` state.
It retains the request while running work is detached and releases it after the
last running turn settles or its runtime stops, fails, or is killed. The policy
does not alter system or desktop idle settings. It does not override explicit
sleep, lid-close behavior, thermal protection, or critical-battery protection.
`system-and-display` can use substantially more battery power.

## Platform behavior

On native Linux, `system` owns a systemd-logind `idle` inhibitor on the system
bus. `system-and-display` additionally owns a cookie from the desktop
`org.freedesktop.ScreenSaver` service on the session bus. WSL is rejected: a
Linux guest cannot inhibit power on its Windows host. Mezzanine neither invokes
a fallback helper nor substitutes a weaker backend when qualifying this path.

On macOS, the backend uses native IOKit assertions. Other platforms report the
request as unavailable. Runtime acquisition failures are nonfatal to agent
work, and a missing display service may leave system-only protection.

The generic confirmed states are `Inactive`, `System`, `SystemAndDisplay`,
`SystemOnly`, and `Unavailable`. They distinguish resources confirmed as held
from a desired configuration.

`mez --json host status` exposes the bounded state for each supervised session
under `sessions[].power_inhibition`. The `--json` flag is global and must appear
before the `host` subcommand. `configured_policy` records the effective user
setting, while `desired_mode` and `desired_generation` record the latest runtime
request. `confirmed_mode`, `confirmed_generation`,
`confirmed_aggregate_state`, the two resource states, `backend_kind`, and
`last_error_class` describe worker-confirmed progress. A lower confirmed
generation means the latest desired request is pending or degraded. The status
never includes raw native errors, D-Bus cookies, file descriptors, IOKit
assertion identifiers, or other host handles. Starting and retained terminal
sessions without a live actor report `power_inhibition` as `null`.

## Real Linux qualification

The checked-in qualification deliberately requires an explicit opt-in because
it calls the production backend against the current desktop session:

```sh
MEZ_REAL_LINUX_POWER_INHIBITION=1 just test-real-linux-power-inhibition
```

The wrapper fails before Cargo starts unless all of these conditions hold:

1. The opt-in value is exactly `1`.
2. The host is Linux and kernel/environment evidence does not indicate WSL.
3. The host is booted with systemd and its system D-Bus is available.
4. `org.freedesktop.login1` owns its system-bus name.
5. A session bus address is present and queryable.
6. `org.freedesktop.ScreenSaver` owns its session-bus name.
7. `busctl`, Cargo, and the configured timeout command are available.

After preflight, the runner starts one ignored test with one libtest thread and
a 120-second outer timeout. The test constructs the production Linux backend,
requires the confirmed state to become `SystemAndDisplay`, requests `Disabled`,
and then requires `Inactive`. Adapter calls also have short internal deadlines.
Any missing prerequisite or weaker result fails clearly; the qualification does
not fall back to fake services, system-only mode, or another backend.

The preflight performs only D-Bus owner queries. The test acquires and releases
Mezzanine-owned inhibitors; neither phase writes idle-delay, lock-screen,
display-power, suspend, lid, or battery settings.

## Troubleshooting

- **WSL rejected:** run the qualification in a native Linux desktop session.
- **No session bus address:** run it from the graphical login session rather
  than a minimal service, container, or unrelated privilege boundary.
- **No ScreenSaver owner:** ensure the current desktop exports the standard
  ScreenSaver D-Bus service. The qualification intentionally does not start a
  replacement service.
- **logind absent or denied:** verify the machine is booted with systemd-logind
  and that the current user may request an `idle` inhibitor.
- **Test reaches `SystemOnly` or `Unavailable`:** inspect host D-Bus policy and
  desktop service health. These are failures for qualification even though
  ordinary agent execution treats inhibition failure as nonfatal.

See the [configuration reference](../configuration/reference.md#agents) for the
field contract.
