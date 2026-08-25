# Mezzanine Manual

This manual explains how to install, configure, operate, and contribute to
Mezzanine. It is organized by user task; [SPEC.md](../SPEC.md) remains the
normative behavior and compatibility contract.

## Start by audience

- **New users:** begin with [Getting started](getting-started/README.md), then
  continue to [Using Mezzanine](using-mezzanine/README.md).
- **Daily users:** use [Using Mezzanine](using-mezzanine/README.md) for panes,
  terminal interaction, agent-shell workflows, and routine sessions.
- **Agent users:** use [Agent and integrations](agent/README.md) for commands,
  continuity, providers, subagents, and MCP.
- **Safety-sensitive users and administrators:** begin with
  [Safety, trust, and security](safety-and-trust/README.md), then review
  [Configuration](configuration/README.md).
- **Operators:** use [Operations and troubleshooting](operations/README.md)
  for lifecycle, diagnostics, recovery, and known symptoms.
- **Contributors:** use [Contributing](contributing/README.md), then consult
  [AGENTS.md](../AGENTS.md) for repository workflow requirements.

## Manual contents

- [Getting started](getting-started/README.md): installation, authentication,
  and a first successful session.
- [Using Mezzanine](using-mezzanine/README.md): sessions, panes, terminal
  input, copy/history, the agent shell, and common workflows.
- [Agent and integrations](agent/README.md): the pane-local agent, commands,
  skills, subagents, context continuity, providers, and MCP.
- [Safety, trust, and security](safety-and-trust/README.md): approvals,
  sandboxing, project trust, instructions, and audit information.
- [Configuration](configuration/README.md): configuration concepts, focused
  topics, the schema reference, and examples.
- [Operations and troubleshooting](operations/README.md): lifecycle, cache
  diagnostics, recovery, and symptom-based guidance.
- [Manual reference](reference-manual/README.md): CLI, key, action, terminal,
  and protocol-reference material, with links to normative contracts.
- [Contributing](contributing/README.md): workspace architecture and local
  development validation, including cross-platform release-load checks.
- [Testing and performance guides](testing/README.md): specialized benchmark
  and release-evidence procedures for contributors and operators.

## Documentation boundaries

Task-oriented section landing pages and chapters state their purpose,
prerequisites, related pages, and next step. Reference pages prioritize stable
contracts and link to their normative source where applicable. A topic has one
canonical owner; other pages summarize it and link to that owner. Relative
links support both repository browsing and published copies.

`docs/reference/` is intentionally outside the published manual. It retains
research, audits, plans, and historical investigations. The published reference
layer is `docs/reference-manual/`, avoiding a collision with that preserved
material.

## Related top-level documents

- [README.md](../README.md): product overview and quick start.
- [SPEC.md](../SPEC.md): normative behavior and compatibility requirements.
- [AGENTS.md](../AGENTS.md): repository workflow and validation rules.
