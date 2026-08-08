# Manual migration inventory

## Purpose

Track the source material, canonical manual owner, and removal conditions for
the published Mezzanine manual. Drafting tasks use this map to prevent missing
features and duplicate documentation; cleanup uses it to decide which legacy
files are safe to remove.

## Scope and exclusions

This inventory covers `README.md`, `SPEC.md` as the normative feature index,
and all user-facing files under `docs/` except `docs/reference/`.
`docs/reference/` contains research, audits, plans, and historical
investigations. It is not a published-manual source and is explicitly excluded
from migration and deletion. `SPEC.md` remains normative and `AGENTS.md`
remains repository workflow guidance; neither is replaced by this manual.

## Source disposition

| Source | Main subjects | Canonical manual destination | Disposition |
| --- | --- | --- | --- |
| `README.md` | Product overview, prerequisites, quick start, daily use, safety summary, providers, CLI, configuration, FAQ, contributor links | `getting-started/`, `using-mezzanine/`, `agent/`, `safety-and-trust/`, `configuration/`, `operations/`, `reference-manual/`, and `contributing/` | Retain as concise onboarding hub; replace duplicated detail with links after manual validation. |
| `docs/README.md` | Manual home and audience navigation | `docs/README.md` | Retain as the canonical manual home. |
| `docs/agent-skills-and-commands.md` | Terminal commands, slash commands, skills, macros, MCP prompt syntax | `agent/commands-skills-and-macros.md`; exhaustive terminal commands and keys link to `reference-manual/` | Rewrite and remove after integration confirms coverage. |
| `docs/context-lifecycle-and-compaction.md` | Context assembly, continuity, compaction, persistence, cache implications | `agent/context-and-continuity.md` | Rewrite for user-observable behavior; remove after integration confirms coverage. |
| `docs/routed-loop-lifecycle.md` | `/loop`, routed workers, handoff, failure, cancellation | `agent/subagents-and-messaging.md` and `using-mezzanine/workflows.md` | Merge and remove after integration confirms coverage. |
| `docs/sandbox-mechanism.md` | Approval versus confinement, Bubblewrap, scopes, network, failures, trust commands | `safety-and-trust/sandboxing.md` | Rewrite and remove after integration confirms coverage. |
| `docs/cache-status-diagnostics.md` | `/status`, cache reuse, continuity diagnostics, provider request shape | `operations/cache-status-and-diagnostics.md` | Migrate and remove after integration confirms coverage. |
| `docs/configuration-reference.md` | Configuration discovery, migrations, schema fields, defaults | `configuration/reference.md` and `configuration/overview.md` | Migrate as the exhaustive reference; remove after integration confirms coverage. |
| `docs/examples/config.toml` | Generated baseline configuration | `docs/examples/config.toml`, linked from `configuration/` | Retain at its existing path unless a later move updates every inbound link. |
| Absent published owners formerly named by the old docs index | CLI, keys, MAAP actions, terminal compatibility, workspace architecture, ownership | `reference-manual/{cli,key-bindings,agent-actions,terminal-compatibility}.md` and `contributing/architecture.md` | Create during drafting; an independent ownership-matrix page is unnecessary unless architecture review finds a distinct maintained artifact. |

## Feature coverage map

The following map derives its feature groups from the current README and
`SPEC.md`. Each row needs one primary owner; supporting pages should summarize
and link rather than reproduce its full reference content.

| Feature group | Normative source | Primary owner | Supporting owners |
| --- | --- | --- | --- |
| Installation, prerequisites, configuration initialization, authentication, first launch | SPEC §§8, 15 | `getting-started/{installation,authentication,first-session}.md` | `configuration/overview.md`, `operations/troubleshooting.md` |
| Sessions, windows, panes, layouts, detach/reattach, snapshots, persistence | SPEC §§5–6, 19 | `using-mezzanine/sessions-and-panes.md` | `operations/lifecycle-detach-and-recovery.md`, `reference-manual/key-bindings.md` |
| Terminal input, command prompt, copy mode, paste buffers, history, notifications | SPEC §7 | `using-mezzanine/terminal-input-copy-and-history.md` | `reference-manual/{cli,key-bindings,terminal-compatibility}.md` |
| Agent shell, prompts, command workflows, plans, skills, macros | SPEC §§7.4–7.5, 10.4–10.5, 11 | `using-mezzanine/agent-shell.md` and `agent/commands-skills-and-macros.md` | `reference-manual/{key-bindings,agent-actions}.md` |
| Agent model, context, action lifecycle, errors, retries, persistence | SPEC §§9–10 | `agent/overview.md` and `agent/context-and-continuity.md` | `operations/cache-status-and-diagnostics.md`, `reference-manual/agent-actions.md` |
| Subagents, local messaging, routed loops, scheduling | SPEC §§10.3, 12, 22 | `agent/subagents-and-messaging.md` | `using-mezzanine/workflows.md` |
| Providers, model profiles, model selection, authentication accounts | SPEC §§15, 23 | `agent/providers-and-models.md` | `getting-started/authentication.md`, `configuration/agents-providers-and-auth.md` |
| MCP integration | SPEC §14 | `agent/mcp-integration.md` | `configuration/extensions-hooks-and-control.md`, `reference-manual/agent-actions.md` |
| Approval, command rules, review, pane protection, bypasses | SPEC §17 | `safety-and-trust/approvals-and-review.md` | `configuration/permissions-sandbox-and-trust.md` |
| Sandbox boundaries, scopes, networking, managed homes, failure behavior | SPEC §§17–18 | `safety-and-trust/sandboxing.md` | `configuration/permissions-sandbox-and-trust.md`, `operations/troubleshooting.md` |
| Project trust and instruction discovery | SPEC §24 | `safety-and-trust/project-trust-and-instructions.md` | `configuration/overview.md` |
| Audit records, security diagnostics, cache diagnostics | SPEC §§18, 26 | `safety-and-trust/audit-and-diagnostics.md` and `operations/cache-status-and-diagnostics.md` | `operations/troubleshooting.md` |
| Configuration schema, precedence, migration, terminal appearance, hooks, extensions, control endpoint | SPEC §8, §§13, 20, 27 | `configuration/{overview,appearance-and-terminal,agents-providers-and-auth,permissions-sandbox-and-trust,extensions-hooks-and-control,reference}.md` | `docs/examples/config.toml` |
| CLI syntax and machine-readable automation | SPEC §§5, 8, 13, 15, 17 | `reference-manual/cli.md` | `using-mezzanine/workflows.md`, `operations/lifecycle-detach-and-recovery.md` |
| Terminal capabilities, width, rendering, compatibility limits | SPEC §§6.7, 25 | `reference-manual/terminal-compatibility.md` | `configuration/appearance-and-terminal.md`, `operations/troubleshooting.md` |
| Workspace architecture, package boundaries, local development and validation | Repository workspace manifest and `AGENTS.md` | `contributing/{architecture,development-and-validation}.md` | `AGENTS.md`, `SPEC.md` |

## Drafting and cleanup acceptance criteria

Before integration can approve legacy cleanup, every mapped source must meet
all of these conditions:

1. Its primary destination exists and covers the user-visible behavior,
   including a normal workflow and a relevant boundary, failure, or limitation.
2. The page declares purpose, prerequisites, related pages, and a next step;
   it uses relative links to its dependencies.
3. Claims about commands, keys, providers, configuration, permissions, and
   lifecycle behavior are checked against `SPEC.md`, generated output, or the
   relevant implementation contract.
4. Exhaustive data belongs only in the canonical reference page; task pages
   link to it rather than duplicate tables.
5. A repository-wide Markdown-link check reports no missing local targets or
   anchors, and a search finds no inbound links to a source approved for
   removal.
6. Configuration documentation verifies its example and schema version against
   generated defaults. The checked-in example and canonical reference both
   declare the current version, 54.

## Cleanup authority

Only the integration task may publish the final superseded-source list. The
cleanup task may delete only legacy files marked “remove after integration
confirms coverage” in this inventory and explicitly included in that final
list. It must preserve `docs/reference/`, `SPEC.md`, `AGENTS.md`, the manual
home, and the configuration example unless an approved replacement and updated
links exist.

## Validated superseded-source list

The following legacy source files have canonical manual owners, no inbound
links outside their own content or this inventory, and are approved for removal
by the cleanup task:

- `docs/agent-skills-and-commands.md`
- `docs/cache-status-diagnostics.md`
- `docs/configuration-reference.md`
- `docs/context-lifecycle-and-compaction.md`
- `docs/routed-loop-lifecycle.md`
- `docs/sandbox-mechanism.md`

## Related pages

- [Manual home](README.md)
- [Getting started](getting-started/README.md)
- [Agent and integrations](agent/README.md)
- [Safety, trust, and security](safety-and-trust/README.md)
- [Configuration](configuration/README.md)

## Next step

Draft each section against this map, then use the integration criteria before
retiring any legacy documentation.
