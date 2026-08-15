# Sandboxing

## Purpose

Explain the operating-system boundary for permitted local shell actions,
including its authority limits, networking behavior, and failure handling.

## Prerequisites

Understand [approvals and review](approvals-and-review.md). A sandbox does not
replace the decision to approve an action.

## Select a backend

`permissions.sandbox = "policy-only"` does not confine filesystem or
shell-network access. Approval policy and optional audit logging remain
separate controls. `bubblewrap` is the current OS-confinement backend and
requires a Linux pane environment. It launches eligible shell workloads in a
constrained Linux namespace and requires a configured `bwrap` executable in
that environment; Mez does not install a privileged helper. Other pane
environments use `policy-only`, which remains an approval and coordination
boundary rather than process isolation.

Use `mez sandbox status --verbose` to inspect configured and effective state,
and `mez sandbox plan` to preview guided setup. The agent-shell `/sandbox`
command exposes pane-local status and narrowly scoped enable/disable controls;
advanced setup and managed-home workflows remain CLI operations.

## Understand effective authority

With Bubblewrap, read scopes are mounted read-only and write scopes are mounted
read-write (and also imply read access). Explicit scopes are the maximum
authority. When no scope is configured, a pane in a trusted project can receive
that project's canonical root as read-write authority; a pane with neither
source has no filesystem authority.

Unavailable configured paths are excluded with a warning rather than silently
broadening authority. The multi-user `/home` root is never usable as an
authority scope. Scope configuration is the sole determinant of filesystem
exposure, including credential-bearing paths, so authorize such paths only
when their exposure is intentional. A permitted home scope is projected through
a private managed home; Mezzanine does not copy or mount the host home,
credentials, or user configuration into it. Configured environment forwarding
names and Git identity can be selectively projected, but they do not grant
filesystem authority and host global Git configuration is not inherited.

## Control network access

`permissions.network_policy` selects the Bubblewrap shell profile: `deny`
isolates every shell action, `allow` connects every action, and `prompt`
connects only an authorized network action. This is a connected-or-isolated
boundary, not destination filtering. Product-owned web, fetch, and MCP actions
are not child shell processes and have their own capability and approval gates.

## Fail safely

Bubblewrap probe, setup, and launch failures stop the action; Mez does not
silently retry on the host. A user may approve one exact unsandboxed retry only
when the retained permission decision permits it. That retry is never automatic
and can have partial effects when the original payload might have started.

`host-access` is a separate primary-user-only approval mode that intentionally
runs local shell work outside Bubblewrap. It should be used only when the host
boundary is explicitly required and understood.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Project trust and instructions](project-trust-and-instructions.md)
- [Configuration](../configuration/README.md)
- [Normative security contract](../../SPEC.md#18-security-and-safety)

## Next step

Review [Project trust and instructions](project-trust-and-instructions.md)
before allowing a project overlay to affect authority.
