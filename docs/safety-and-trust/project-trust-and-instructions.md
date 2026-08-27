# Project trust and instructions

## Purpose

Explain how Mezzanine finds repository instructions and why project
configuration requires a separate trust decision.

## Prerequisites

Know the project root and inspect unfamiliar repository files before granting
trust.

## Instruction discovery

By default, Mezzanine searches for `AGENTS.md` through the pane shell. The
project root is the nearest ancestor containing a `.git` directory or file;
when none exists, it is the pane working directory. For a path-scoped task,
applicable instruction files are collected from the project root down to the
target directory.

`instructions.project_filenames` can add ordered alternatives to the default
`AGENTS.md` name. In each directory, Mezzanine selects at most one instruction
file: the first configured filename that exists. Directory depth then controls
scope and descendant precedence as described below.

Ancestor guidance applies before descendant guidance, and a descendant file
takes precedence when instructions conflict. Its scope is the directory tree
containing it. Mezzanine applies every applicable instruction to a modified
file, while direct user instructions and system or safety requirements retain
their higher precedence.

Instruction content is model-visible workflow guidance, not permission to
change security, credentials, tool availability, or hidden policy. Mezzanine
marks the source and scope, and reports when configured size limits omit or
truncate content.

## Trust a project overlay

Project configuration under `.mezzanine/config.toml`, `.mezzanine/config.yaml`,
`.mezzanine/config.yml`, or `.mezzanine/config.json` remains pending until the
primary user explicitly trusts or rejects the project root. Inspect the overlay
and applicable instructions first. The trust store records trusted, rejected,
and revoked roots; inspect a root before changing its decision:

```sh
mez sandbox trust list
mez sandbox trust inspect PATH
mez sandbox trust add PATH
mez sandbox trust reject PATH
mez sandbox trust revoke PATH
```

`add` marks a root trusted, `reject` records that its overlay must not apply,
and `revoke` removes the prior trust decision from effect. The agent-shell
`/sandbox trust` flow can decide an explicit or pending root from the active
pane. Trust decisions persist in the user-private trust store. Trusting an
overlay does not itself grant host access, disable approval, or override a
sandbox boundary.

Even after trust, project overlays cannot change primary-user-only execution
authority: approval policy or bypass, sandbox backend, read/write scopes,
network and destructive-action policy, sandbox authority, host/transport
settings, or model-profile approval policy. Trust activates only otherwise
eligible project settings, such as hooks, MCP/provider configuration, and
project command rules. Separately, a trusted project root can provide the
default sandbox project scope when no user scopes are configured.

While an applicable overlay is pending, new agent turns wait for the primary
user to trust or reject it rather than silently substituting lower-precedence
project behavior. If no primary client is attached, the request remains
pending until one can decide it. Use `mez config layers` to distinguish an
applied overlay from one that is pending or ignored.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Sandboxing](sandboxing.md)
- [Configuration](../configuration/README.md)
- [Normative instruction-discovery contract](../../SPEC.md#24-project-instruction-discovery)

## Next step

Use [Audit and diagnostics](audit-and-diagnostics.md) to inspect records when a
policy, trust, or action outcome needs investigation.
