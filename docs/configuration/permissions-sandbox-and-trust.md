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
optional audit logging remain separate controls. `bubblewrap` enforces its
configured filesystem and network boundary for eligible local shell work.
`host-access` is a primary-user-only approval mode that runs local shell work
outside Bubblewrap.

Read scopes are maximum read authority; write scopes also imply reads. Network
policy controls connected versus isolated Bubblewrap profiles, not destination
filtering. Keep rules narrow: an exact command or digest rule is safer than a
broad prefix rule. Do not store credentials in scope paths or use a trusted
project overlay to attempt to broaden the primary user's execution boundary.

## Trust overlays separately

Project configuration and project skills/macros are discovered under the active
project root but remain pending until a primary user trusts or rejects that
root. Use `mez sandbox trust list` to inspect decisions. Trust enables eligible
overlay behavior; it does not approve an action or expand sandbox authority by
itself.

## Related pages

- [Project trust and instructions](../safety-and-trust/project-trust-and-instructions.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [Configuration reference](reference.md)

## Next step

Use [Extensions, hooks, and control](extensions-hooks-and-control.md) for
integrations that operate outside ordinary pane-shell work.
