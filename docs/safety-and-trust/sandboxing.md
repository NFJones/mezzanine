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
separate controls. `bubblewrap` provides Linux namespace confinement.
`seatbelt` provides macOS operation-level mandatory access control through the
fixed `/usr/bin/sandbox-exec` interface. Seatbelt keeps the host mount, PID,
user, IPC, UTS, and network namespaces visible; it must not be treated as
Bubblewrap-equivalent namespace isolation. Apple deprecates the public
`sandbox-exec`/SBPL interface, so future macOS releases may make this backend
unavailable.

New Linux configuration selects `full-access + bubblewrap` only when
`/usr/bin/bwrap` is an executable regular file. New macOS configuration selects
`full-access + seatbelt` only when `/usr/bin/sandbox-exec` is executable. Either
platform selects `auto-allow + policy-only` when its fixed executable is absent.
Existing configurations are preserved by migration. Presence is only a setup
decision: every sandboxed workload still requires the exact runtime capability
probe and fails closed if that proof cannot be established.

Pane shell mode obtains environment and authority evidence through the pane
shell before probing and launching. Native mode derives equivalent evidence
from the live pane root process and host metadata, then probes and launches
outside the pane. Both modes compile the same backend policy. Neither backend
uses a privileged helper, and an unavailable executable or failed probe never
silently changes execution to `policy-only` or the host.

Use `mez sandbox status --verbose` to inspect configured and effective state,
including backend, executable, capability, profile, managed-home, network, and
namespace facts. The JSON form is workflow schema version 2. Use `mez sandbox
plan` to preview the platform-selected backend and fixed-executable presence.
The agent-shell `/sandbox`
command exposes pane-local status and narrowly scoped enable/disable controls;
advanced setup and managed-home workflows remain CLI operations.

## Understand effective authority

User-configured read scopes are maximum read authority and write scopes also
imply reads. Bubblewrap projects those scopes as read-only or read-write mounts.
Seatbelt leaves paths in the host namespace and permits or denies file
operations against canonical authorized paths. Effective authority
can additionally include code-owned user `skills` and `macros` roots. When no
user read or write scope is configured, a pane in a trusted project is intended
to receive that project's canonical root as read-write authority; a pane with
neither source has no project filesystem authority.

Unavailable configured paths are excluded with a warning rather than silently
broadening authority. The multi-user `/home` root is never usable as an
authority scope. Effective scopes—not approval or project instructions—define
filesystem exposure, including credential-bearing paths, so authorize such
paths only when intentional. Trusted-project runs use backend/profile-keyed
private managed homes when a private configuration root is available.
Bubblewrap mounts its managed home at a synthetic in-sandbox home path.
Seatbelt uses its private canonical host path directly as `HOME` while denying
operations outside authorized paths. Neither backend copies the real host home,
credentials, or global Git configuration. Cleanup and quota remain user or
deployment policy. Configured environment forwarding names and sanitized Git
identity do not grant filesystem authority.

## Control network access

`permissions.network_policy` selects whether a shell action may use networking.
With Bubblewrap, `deny` uses an isolated network namespace. With Seatbelt,
`deny` rejects TCP, UDP, and Unix-domain socket operations in the visible host
namespace. `allow` permits networking and `prompt` permits it only after the
action's network requirement is authorized. Neither backend provides
destination filtering. Product-owned web, fetch, and MCP actions are not child
shell processes and have their own capability and approval gates.

## Fail safely

Probe, profile, authority, setup, and launch failures stop the action; Mez does
not silently retry on the host. Trusted lifecycle evidence is separate from
payload output. Only separately proven eligible pre-payload failures may offer
one exact approval-gated unsandboxed retry. An established nonzero payload may
receive one bounded sandbox-failure assessment. No retry is automatic, and a
warning is required when partial effects may already exist.

`host-access` is a separate primary-user-only approval mode that intentionally
runs local shell work outside the configured sandbox. It should be used only
when the host boundary is explicitly required and understood.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Project trust and instructions](project-trust-and-instructions.md)
- [Configuration](../configuration/README.md)
- [Normative security contract](../../SPEC.md#18-security-and-safety)

## Next step

Review [Project trust and instructions](project-trust-and-instructions.md)
before activating a project overlay or using a trusted-project default scope.
