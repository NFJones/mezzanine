# Permissions, sandbox, and trust

## Purpose

Configure approval policy, command rules, sandbox authority, network behavior,
and project trust without conflating those separate controls.

## Prerequisites

Read [Approvals and review](../safety-and-trust/approvals-and-review.md) and
[Sandboxing](../safety-and-trust/sandboxing.md) before changing these settings.

## Set a deliberate execution boundary

The `permissions` table owns approval policy, command rules, destructive-action
policy, network policy, sandbox backend, scopes, and the explicit bypass mode.
`policy-only` provides no operating-system confinement; approval policy and
optional audit logging remain separate controls. `bubblewrap` provides Linux
namespace confinement; `seatbelt` provides macOS operation-level confinement
in the visible host namespace. When
`/usr/bin/bwrap` is an executable regular file, generated Linux configuration
pairs `full-access` approval with Bubblewrap; otherwise it pairs `auto-allow`
with `policy-only`. When `/usr/bin/sandbox-exec` is executable, generated macOS
configuration pairs `full-access` with Seatbelt; otherwise it pairs
`auto-allow` with `policy-only`. Existing configurations are not auto-enabled
by migration. Pane shell mode prepares the selected backend through the pane
shell; native shell mode derives identity,
environment, working directory, and canonical path authority from the pane
root process and host metadata without pane input. Executable presence does not
prove capability: the exact runtime probe remains mandatory and fail-closed.
Runtime-owned web, fetch, and MCP actions are separate capability
and approval boundaries rather than child shell processes. `host-access` is a
primary-user-only approval mode that runs local shell work outside the selected
sandbox.

`permissions.bypass_mode` is visible configuration state, but configuration
cannot enable it. Only an explicit primary-user bypass decision can do so; see
[Approvals and review](../safety-and-trust/approvals-and-review.md) before
relying on any reduced gating.

Read scopes are maximum read authority; write scopes also imply reads.
Bubblewrap realizes them as mounts, while Seatbelt enforces canonical host-path
operations without hiding namespaces. Network policy controls isolated versus
connected Bubblewrap profiles or denied versus permitted Seatbelt operations,
not destination filtering. Keep rules narrow: an exact command or digest rule is safer than a
broad prefix rule. Do not store credentials in scope paths or use a trusted
project overlay to attempt to broaden the primary user's execution boundary.

## Trust overlays separately

Project configuration and project skills/macros are discovered under the active
project root but remain pending until a primary user trusts or rejects that
root. Use `mez sandbox trust list` to inspect decisions. Trust enables eligible
overlay behavior and project skill/macro discovery; it does not approve an
action, treat project content as trusted input, or expand sandbox authority by
itself.

## Related pages

- [Project trust and instructions](../safety-and-trust/project-trust-and-instructions.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration reference](reference.md)

## Next step

Use [Extensions, hooks, and control](extensions-hooks-and-control.md) for
integrations that operate outside ordinary pane-shell work.
