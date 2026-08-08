# Project trust and instructions

## Purpose

Explain how Mezzanine finds repository instructions and why project
configuration requires a separate trust decision.

## Prerequisites

Know the project root and inspect unfamiliar repository files before granting
trust.

## Instruction discovery

By default, Mezzanine searches for `AGENTS.md` through the pane shell. The
project root is the nearest ancestor containing `.git`; when none exists, it is
the pane working directory. For a path-scoped task, applicable instruction
files are collected from the project root down to the target directory.

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
Bubblewrap boundary.

While a relevant overlay is pending and no primary client can decide it, Mez
does not silently substitute lower-precedence behavior for work that depends on
that overlay. Agent prompts and turns, hooks, MCP configuration, command rules,
and provider settings scoped to that project wait for the trust decision. Use
`mez config layers` to distinguish an applied overlay from one that is pending
or ignored.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Sandboxing](sandboxing.md)
- [Configuration](../configuration/README.md)
- [Normative instruction-discovery contract](../../SPEC.md#24-project-instruction-discovery)

## Next step

Use [Audit and diagnostics](audit-and-diagnostics.md) to inspect records when a
policy, trust, or action outcome needs investigation.
