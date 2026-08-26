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

## Manage durable leases and recovery

Remote session assignments are durable leases, not live process guarantees.
Inspect them through local Unix administration:

```console
mez lease list --all
mez lease show TARGET
mez lease checkpoint TARGET
mez lease recover TARGET
```

Checkpoint and recovery are generation-fenced. A host restart does not preserve
PTYs or child processes; recovery reconstructs a compatible checkpoint into
fresh processes. Releasing a lease, revoking a lease, killing a live runtime,
and revoking device trust are separate operations. Active release or revocation
requires the explicit `--terminate` option, and garbage collection previews by
default. See the [CLI reference](../reference-manual/cli.md#persistent-host-command-contract)
for the complete lease command contract.

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
