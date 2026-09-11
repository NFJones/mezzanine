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

Newly discovered project configuration under `.mezzanine/config.toml`,
`.mezzanine/config.yaml`, `.mezzanine/config.yml`, or
`.mezzanine/config.json` remains pending until the primary user explicitly
trusts or rejects the project root. A previously trusted root applies overlays
discovered under that canonical root unless trust was revoked or policy
requires renewed approval. A deeper rejected or revoked nested decision stops
overlays under that nested root from applying even when a broader ancestor is
trusted. Inspect the overlay and applicable instructions
directly before deciding. The trust store records trusted, rejected, and
revoked roots; inspect its persisted record before changing a decision:

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

### Overlay discovery versus implicit filesystem authority

Project discovery finds the nearest repository marker and the overlay files it
can reach. Discovery decides which overlay may apply and which root is prompted
for review; it never grants or withholds filesystem authority by itself.
Implicit trusted-project authority comes only from the deepest stored trust
decision governing the working directory. A trusted parent with a deeper trusted
nested repository keeps the nested root as the effective scope, while a deeper
rejected or revoked decision withholds implicit authority for that nested root
even though the parent stays trusted. A nested repository marker with no stored
decision keeps the recursive parent trust, because Mezzanine never invents a
rejection from the presence of `.git`. Overlay application uses the same
resolution against each overlay file's own directory, so a nested directory
without a repository marker of its own still stops applying overlays once the
deepest stored decision for that directory is rejected or revoked. Decisions are
read from the same trust-policy and configuration-schema versions as the strict
project lookup: a record written under another version counts as no decision at
all instead of granting or withholding authority. Rejection and revocation
withhold only
the implicit default: explicitly configured `permissions.read_scopes` and
`permissions.write_scopes` keep working unchanged. The effective permission
status reports the withheld provenance (`project-trust-rejected`,
`project-trust-revoked`, or `project-trust-pending`) together with the governing
root, so a withheld decision is distinguishable from an undecided project, and
`mez sandbox status` reports the same withheld provenance and governing root.
Admission of a shell command or semantic patch denies a withheld decision for
both action types, and a pending decision is a blocking denial rather than a
correctable action argument.

The current `mez sandbox trust inspect` CLI output is limited to the persisted
trust record: canonical root, decision, Git marker, decision time, schema and
trust-policy versions, and recorded VCS remote. It does **not** currently show
the discovered overlay files, validation diagnostics, or capability-expansion
summary required by the normative `ProjectTrustState` contract. Until that
implementation gap is closed, inspect the overlay files themselves and use
`mez config layers` to determine whether each layer is applied, pending, or
ignored; do not treat the trust-record output as an overlay-content review.

The direct `mez sandbox trust add`, `reject`, and `revoke` commands currently
write the user-private trust store without proving that an attached primary
client made the decision. This differs from the normative requirement that
trust and rejection decisions require the primary client. Treat local account
access to these commands as security-sensitive, and prefer the pane-local
`/sandbox trust` decision flow when an attached primary is available.

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
