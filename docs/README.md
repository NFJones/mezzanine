# Mezzanine Documentation Guide

This directory is the stable entry point for Mezzanine user-facing and
reference documentation.

## Start here by audience

- **New users**: begin with the repository [README](../README.md) for product
  overview, quick start, core workflows, and the first successful agent task.
- **Daily users and operators**: use
  [Agent skills and commands](agent-skills-and-commands.md) for the command
  surfaces and [Configuration reference](configuration-reference.md) for exact
  configuration fields and defaults.
- **People validating terminal behavior**: use
  [Terminal compatibility coverage](terminal-compatibility.md) together with
  [SPEC.md Section 25](../SPEC.md#25-terminal-compatibility-test-suite).
- **Contributors**: use [AGENTS.md](../AGENTS.md) for repository workflow,
  testing, and handoff requirements.
- **Specification readers**: use [SPEC.md](../SPEC.md) as the normative source
  for behavior, especially configuration, agent capabilities, shell commands,
  permissions, and persistence.

## Stable documents in this tree

- [configuration-reference.md](configuration-reference.md): generated default
  configuration, supported fields, and layer behavior.
- [agent-skills-and-commands.md](agent-skills-and-commands.md): the three
  interactive command surfaces, explicit skill usage, and common operator
  workflows.
- [terminal-compatibility.md](terminal-compatibility.md): coverage summary for
  the xterm-compatible terminal profile.
- [examples/config.toml](examples/config.toml): the generated baseline config
  example.

## Related top-level docs

- [README.md](../README.md): onboarding hub and daily workflow guide.
- [SPEC.md](../SPEC.md): normative behavior and compatibility requirements.
- [AGENTS.md](../AGENTS.md): contributor workflow and validation rules.
