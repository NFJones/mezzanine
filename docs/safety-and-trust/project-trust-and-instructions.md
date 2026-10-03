# Project trust and instructions

## Purpose

Review repository guidance and project configuration before enabling behavior
that can run commands, contact services, or expose project files.

## Prerequisites

Know the canonical project root. Read unfamiliar instructions and configuration
before making a trust decision; a Git marker is not proof of safety.

## Understand repository instructions

By default, Mezzanine discovers `AGENTS.md` through the pane shell. The project
root is the nearest ancestor with a `.git` directory or file, or the pane working
directory when none exists. For a path-scoped task, applicable guidance spans
the project root and target directory. `instructions.project_filenames` selects
ordered filenames; each directory contributes at most the first existing file.
Hidden-directory discovery and content limits are configurable.

Ancestor guidance applies before descendant guidance; descendant guidance takes
precedence within its subtree. Mezzanine marks source and scope and reports
omitted or truncated content. Check those warnings if expected workflow guidance
is missing. Guidance is model-visible instruction, **not enforced filesystem
policy**. It cannot grant credentials, tools, host access, or permission to
override security requirements. Do not rely on an instruction such as “never
read secrets” as a substitute for scopes and confinement.

## Review a project overlay

Project configuration is discovered under `.mezzanine/config.toml`,
`.mezzanine/config.yaml`, `.mezzanine/config.yml`, or `.mezzanine/config.json`.
Newly discovered roots require a trust decision before applicable overlays are
used. While an overlay is pending, new agent turns wait for a primary user's
decision rather than silently using lower-precedence project behavior. Without
an attached primary, that request remains pending.

Before trusting:

1. Inspect the actual overlay files and applicable repository instructions.
2. Review hooks, command rules, providers, and MCP configuration, including
   executables, endpoints, and credential references. Eligible hooks and
   connectors can have host-side effects outside a child shell sandbox.
3. Run `mez config layers` to identify applied, pending, and ignored layers.
4. Inspect `/permissions` and `mez sandbox status --verbose` for effective roots
   and confinement. Trust can supply default project write authority as described
   below; it is not only a configuration-loading choice.
5. Prefer the pane-local `/sandbox trust` flow for an attached-primary decision.

Trust enables otherwise eligible project settings. It cannot let an overlay
change primary-user-only execution authority: approval policy or bypass,
sandbox backend/authority, read/write scopes, network or destructive-action
policy, host/transport settings, or model-profile approval policy.
Nevertheless, activating hooks, providers, MCP servers, or project command
rules deserves security review. Trust is not certification that their behavior
is safe.

## Inspect and change stored decisions

```sh
mez sandbox trust list
mez sandbox trust inspect PATH
mez sandbox trust add PATH
mez sandbox trust reject PATH
mez sandbox trust revoke PATH
```

`add` marks a root trusted; `reject` records that its overlays must not apply;
`revoke` removes prior trust from effect while retaining a revoked decision.
Decisions persist in the user-private trust store. Do not edit it by hand.

**Current inspection limit:** `trust inspect` shows the stored root, decision,
Git marker, time, versions, and recorded VCS remote. It does not show discovered
overlay contents, validation diagnostics, or the capability-expansion summary
required by SPEC. Use direct file review and `mez config layers`; the stored
record is not an overlay-content review. An absent exact record also does not
rule out recursive trust from an ancestor.

**Current CLI authority limit:** direct `add`, `reject`, and `revoke` write the
trust store without verifying an attached primary client, unlike SPEC's
primary-client requirement. Treat access to the local account and these commands
as security-sensitive. The pane-local flow is preferable when available, but
it does not prevent other same-account processes from using the direct CLI.

## Distinguish trust from filesystem authority

When both `permissions.read_scopes` and `permissions.write_scopes` are empty,
a trusted root supplies default project read/write authority. The deepest
stored decision governing the working directory wins:

- A deeper trusted root narrows the implicit project root to that root.
- A deeper rejected or revoked root withholds implicit authority even under a
  trusted ancestor; a pending decision also blocks shell and patch admission.
- A nested repository with no decision retains recursive parent trust. Merely
  adding `.git` does not revoke it.
- Explicit configured scopes are independent grants. Rejecting or revoking a
  project does not remove them.

Overlay eligibility is resolved against each overlay's own directory, so a
nested rejection can stop its overlays even without a separate Git marker.
Stored records from incompatible trust-policy or configuration-schema versions
do not count as current decisions. Check effective status after an upgrade
rather than assuming an old record still grants authority.

Status reports withheld provenance as `project-trust-rejected`,
`project-trust-revoked`, or `project-trust-pending`, together with its governing
root. These admission checks also apply to shell and patch work under
`policy-only`, native shell mode, and an approved sandbox fallback. They are not
OS confinement: under `policy-only`, an admitted shell is not physically
restricted to project paths. See [Sandboxing](sandboxing.md).

Revocation is not an undo operation. Review running work and already-applied
effects separately; do not assume it rolls back files, terminates every
connector, or removes explicit grants.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Sandboxing](sandboxing.md)
- [Configuration](../configuration/README.md)
- [Normative instruction-discovery contract](../../SPEC.md#24-project-instruction-discovery)

## Next step

Confirm the layer state and effective scopes after any trust decision, then use
[Audit and diagnostics](audit-and-diagnostics.md) if the outcome is unexpected.
