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
`auto-allow` with `policy-only`. Generation also sets `network_policy = "allow"`
for Bubblewrap or Seatbelt; policy-only generation keeps `"prompt"`. Other
platforms generate `ask` with `policy-only`. Prefer `mez config init` to copying
the portable example's `ask`/`bubblewrap`/`prompt` tuple unchanged. Existing
configurations are not auto-enabled by migration.

Pane shell mode prepares the selected backend through the pane
shell; native shell mode derives shell identity, working directory, and
canonical path authority from the pane root process and host metadata without
pane input. Optional values named by `permissions.env_whitelist` instead come
from the immutable Mez server-startup environment snapshot, not the active
pane's environment. Native workloads compose a cleared-base environment from
validated identity/path evidence, documented runtime requirements, and those
selected startup values; unset or unsafe values are omitted with redacted
diagnostics. An explicit `[]` forwards none of the optional values. The same
selection applies to ordinary Bubblewrap and Seatbelt workloads; internal
semantic `apply_patch` phases retain their fixed environment. Executable
presence does not prove capability: the exact runtime probe remains mandatory
and fail-closed.
Runtime-owned web, fetch, and MCP actions are separate capability
and approval boundaries rather than child shell processes. `host-access` is a
primary-user-only approval mode that runs local shell work outside the selected
sandbox.

`permissions.bypass_mode` is visible configuration state, but configuration
cannot enable it. Only an explicit primary-user bypass decision can do so; see
[Approvals and review](../safety-and-trust/approvals-and-review.md) before
relying on any reduced gating.

With an active OS sandbox, read scopes are maximum read authority; write scopes
also imply reads. Under `policy-only`, scopes are advisory approval and
coordination metadata, not filesystem enforcement.
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
action or make arbitrary project content safe. When both configured read and
write scope lists are empty, however, a trusted root supplies default project
read/write authority. Review that grant before trusting a repository. Explicit
scopes remain independent grants; project overlays cannot broaden the primary
user's execution boundary. See [Project trust and instructions](../safety-and-trust/project-trust-and-instructions.md)
for nested decisions, revocation, and the distinction from OS confinement.

## Related pages

- [Project trust and instructions](../safety-and-trust/project-trust-and-instructions.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration reference](reference.md)

## Next step

Use [Extensions, hooks, and control](extensions-hooks-and-control.md) for
integrations that operate outside ordinary pane-shell work.
