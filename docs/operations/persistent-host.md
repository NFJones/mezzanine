# Persistent multi-session host

## Purpose

Run `mez host serve` as the long-lived owner of local session discovery,
creation, routing, durable leases, and optional host-scoped Iroh access.

## Prerequisites

- Complete [Getting started](../getting-started/README.md).
- Keep the user-private Unix control path available for administration and
  recovery.
- Run the host as the same unprivileged account that owns the Mezzanine
  configuration and will administer its sessions; do not run it as root to
  bypass path or permission failures.
- Use a service manager that preserves standard output and standard error when
  deploying the host as a background service.

## Choose the session model

Mezzanine has two foreground service commands with different ownership:

- `mez serve` runs one foreground session service. It is the direct-session
  compatibility path and does not become a multi-session host.
- `mez host serve` runs the persistent host. It supervises independent session
  runtimes and is the intended service-manager command.

Ordinary local commands use the persistent host when its default Unix socket is
already available. An explicit `-S PATH` or `-L NAME` target selects a direct
session socket instead of host routing.

## Start and inspect the host

Validate configuration, then start the host in the foreground:

```console
mez config validate
mez host serve
```

Explicit `mez host serve` does not require `host.enabled = true`. That setting
controls whether ordinary local commands may auto-start the host; it does not
prevent an operator or service manager from starting the host explicitly.

The host writes an initial machine-readable readiness record to standard
output and operational diagnostics to standard error. A service manager should
run exactly one foreground instance for the account, provide a stable `HOME`
and runtime-directory environment, capture both streams, restart the process
according to local policy, and stop it gracefully rather than treating it as an
interactive attachment. Do not infer readiness merely from process creation;
retain and inspect the initial readiness record or use `mez host status`.

From another local process, inspect or stop the service:

```console
mez host status
mez host reconcile
mez host stop --timeout 10
```

`reconcile` prunes stale compatibility discovery records. It is not a general
repair or lease-deletion command.

## Run the host with systemd

Run the host as the regular, unprivileged account that owns its configuration
and sessions. The following system service is a starting point; replace the
user, group, home directory, and installed binary path with the values for the
host account:

```ini
# /etc/systemd/system/mez-host.service
[Unit]
Description=Mezzanine persistent host
Wants=network-online.target
After=network-online.target

[Service]
Type=simple
User=YOUR_USER
Group=YOUR_GROUP
Environment=HOME=/home/YOUR_USER
Environment=MEZ_TMPDIR=/run/mez
RuntimeDirectory=mez
RuntimeDirectoryMode=0700
ExecStart=/home/YOUR_USER/.cargo/bin/mez host serve
Restart=on-failure
RestartSec=2

# Do not pass capabilities through exec to Bubblewrap workloads.
AmbientCapabilities=

[Install]
WantedBy=multi-user.target
```

Install and start it with:

```console
sudo systemctl daemon-reload
sudo systemctl enable --now mez-host.service
sudo systemctl status mez-host.service
```

Use the same runtime-directory selection in every administering or attaching
shell for this unit:

```sh
export MEZ_TMPDIR=/run/mez
mez host status
```

`MEZ_TMPDIR` takes precedence over other runtime-directory settings and selects
`/run/mez/mez-<uid>` for this example. Without that export, a normal Linux login
shell usually selects `$XDG_RUNTIME_DIR/mez` instead and can miss the running
host or start a separate direct-session daemon. Do not use `-S` to target
`host.sock`: that selector expects a session control socket and bypasses host
routing. Keep the same configuration root as the service as well. Using
`MEZ_TMPDIR` avoids replacing the desktop session's `XDG_RUNTIME_DIR`; it does
not supply a graphical session bus or clipboard environment to the service.

Use an unprivileged UDP port for optional Iroh service. Do not routinely grant
file or ambient capabilities to the general-purpose `mez` executable merely
to bind a reserved port. Ambient capabilities can also make a non-setuid
Bubblewrap reject its capability state. If a deployment truly needs a reserved
port, have its administrator design and qualify the privilege boundary rather
than treating executable capabilities as a sandbox-compatible default.

This unit is Linux-specific. On macOS, use the deployment's launchd policy to
run the same foreground command as the unprivileged account, preserve logs,
and arrange graceful shutdown. Do not copy Linux capability settings to macOS.

## Enable local auto-start

To let an ordinary default-target command start the host when it is absent,
enable the host policy in primary-user configuration:

```console
mez config set host.enabled true
mez config set host.auto_start_local true
mez config validate
```

With both values enabled, concurrent local callers elect at most one bounded
host startup. If auto-start is disabled and no host is running, ordinary local
commands retain the direct-session behavior. If a host is already running,
default-target local commands use it even when later configuration disables
future auto-start.

## Create, list, and attach sessions

Once the host is running, the familiar local commands route through it:

```console
mez                         # attach an eligible session, or create one
mez new --name project-a    # always create a new supervised session
mez list                    # list resumable hosted sessions
mez attach SESSION_TARGET   # attach an existing listed session
mez list --all              # also include visible remote durable leases
```

Use the identifier shown by `mez list` when selecting a session. `mez attach`
with an explicit target does not silently create a replacement. The host
routes a connection to one session runtime; pane, client, terminal, agent, and
presentation state remain isolated in that runtime.

## Manage durable leases

Remote session assignments are durable leases, not live process guarantees.
Inspect them through local Unix administration:

```console
mez lease list --all
mez lease show TARGET
```

Remote leases retain durable reservations, but remote sessions are not
checkpointed or reconstructed after a host restart; interrupted active leases
are reported as failed until released or garbage-collected. Hosted-local
assignments retain their separate snapshot recovery path. Releasing a lease,
revoking a lease, killing a live runtime, and revoking device trust are separate
operations. Active release or revocation requires the explicit `--terminate`
option, and garbage collection previews by default. See the [CLI
reference](../reference-manual/cli.md#persistent-host-command-contract) for the
complete lease command contract.

Hosted-local restart reconciliation preserves the stored update timestamp when
the wall clock moves backward, while advancing boot and assignment generations.
Valid checkpoints remain eligible for recovery; old runtime authority does not.

## Stop, upgrade, and recover

Detaching a client is not stopping the host. A host stop or service-manager
restart interrupts its supervised sessions, pane processes, and agent work.
Hosted-local checkpoints recover layout/state, not the old live processes;
remote active leases are not reconstructed after restart.

Before a planned restart:

1. Inspect `mez list --all` and `mez lease list --all`; notify attached users.
2. Let consequential agent work settle, or stop it and record uncertain effects.
3. Preserve private configuration and needed session data. Stop the owning host
   before copying remote identity and trust together; protect backups as secrets.
4. Validate configuration and use graceful shutdown. Review checkpoint or
   shutdown errors rather than immediately forcing termination.
5. After restart, check `mez host status`, session discovery, local attach, and
   lease state. Review interrupted work before retrying non-idempotent actions.

Read [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md) for
snapshots and conversation recovery. An automatic service restart restores
availability, not proof that interrupted work completed safely.

## Add remote access deliberately

Local use does not require Iroh. Enable and validate host-scoped Iroh policy
only after Unix administration and recovery work. Then create a role-limited
invitation over local Unix control and pair each client device explicitly. See
[Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
for the pairing workflow and [Iroh production operations and
rollout](iroh-production-operations-and-rollout.md) before relying on remote
access outside controlled development.

## Diagnose startup and routing

If a host cannot be reached:

1. Run `mez config validate` and `mez config layers`.
2. Run `mez host serve` in the foreground to retain its startup diagnostic.
3. Check `host.enabled` and `host.auto_start_local` only when automatic startup
   is expected; neither is required for explicit foreground startup.
4. Use `-S` or `-L` only when intentionally bypassing host routing for a direct
   session.
5. Preserve service-manager output before restarting or reconciling records.

## Related pages

- [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md)
- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
- [Configuration reference](../configuration/reference.md#host)
- [CLI reference](../reference-manual/cli.md#persistent-host-command-contract)

## Next step

Configure the service manager around `mez host serve`, verify local session
creation and reattachment, and only then consider optional remote access.
