# Mezzanine Documentation Guide

This directory is the stable entry point for Mezzanine user-facing and
reference documentation.

## Start here by audience

- **New users**: begin with the repository [README](../README.md) for product
  overview, quick start, core workflows, and the first successful agent task.
- **Daily users and operators**: use
  [Agent skills and commands](agent-skills-and-commands.md) for the command
  surfaces, [MAAP action reference](maap-actions-reference.md) for structured
  agent actions, and [Configuration reference](configuration-reference.md) for
  exact configuration fields and defaults.
- **Contributors**: use [AGENTS.md](../AGENTS.md) for repository workflow,
  testing, and handoff requirements, and
  [Workspace architecture](workspace-architecture.md) for package ownership and
  dependency direction.
- **Specification readers**: use [SPEC.md](../SPEC.md) as the normative source
  for behavior, especially configuration, agent capabilities, shell commands,
  permissions, and persistence.

## Stable documents in this tree

- [configuration-reference.md](configuration-reference.md): generated default
  configuration, supported fields, and layer behavior.
- [terminal-compatibility-matrix.md](terminal-compatibility-matrix.md):
  advertised terminal capabilities, current regression coverage, unsupported
  behavior, and full-screen TUI fixture backlog.
- [agent-skills-and-commands.md](agent-skills-and-commands.md): the three
  interactive command surfaces, explicit skill usage, and common operator
  workflows.
- [cache-status-diagnostics.md](cache-status-diagnostics.md): cumulative and
  latest-request cache reuse, immutable-context continuity, and trace fields.
- [context-lifecycle-and-compaction.md](context-lifecycle-and-compaction.md):
  stable, chronological, and volatile context ownership plus execution-group
  compaction and settlement rules.
- [workspace-architecture.md](workspace-architecture.md): workspace package
  ownership, dependency edges, and boundary rules.
- [workspace-ownership-matrix.md](workspace-ownership-matrix.md): audited
  module ownership, final adapter surfaces, and decomposition acceptance
  evidence.
- [examples/config.toml](examples/config.toml): the generated baseline config
  example.

## Related top-level docs

- [README.md](../README.md): onboarding hub and daily workflow guide.
- [SPEC.md](../SPEC.md): normative behavior and compatibility requirements.
- [AGENTS.md](../AGENTS.md): contributor workflow and validation rules.
