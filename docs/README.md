# Mezzanine Manual

This manual explains how to install, configure, and use Mezzanine. Start with
the task that matches your needs; ordinary terminal use does not require an
agent provider or authentication. Contributor guidance and protocol details
are separate sections for people developing Mezzanine or integrations.

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
  for persistent-host deployment, lifecycle, diagnostics, recovery, and known
  symptoms.
- **Remote users:** use [Remote pairing and
  recovery](safety-and-trust/remote-pairing-and-recovery.md) for Iroh pairing,
  reconnect profiles, revocation, and recovery boundaries. For graphical
  applications, continue to the [X11 forwarding
  workflow](using-mezzanine/workflows.md#forward-x11-applications-from-a-remote-session).
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
- [Operations and troubleshooting](operations/README.md): persistent-host
  operation, lifecycle, cache diagnostics, recovery, and symptom-based
  guidance.
- [Manual reference](reference-manual/README.md): CLI commands, keys, agent
  actions, and terminal behavior. Its protocol subsection is for integration
  implementers, not a prerequisite for using Mezzanine.
- [Contributing](contributing/README.md): workspace architecture and local
  development validation, including cross-platform release-load checks.

## Documentation boundaries

The getting-started, usage, agent, safety, configuration, and operations
chapters target users and administrators. You do not need to read source code,
wire protocols, or repository workflow rules to follow them. Use the command
references when you need syntax rather than a guided workflow.

[Contributing](contributing/README.md) is for developers and maintainers;
[Protocol reference](reference-manual/protocols/README.md) is for integration
implementers. [SPEC.md](../SPEC.md) defines normative requirements, but some
requirements are not yet implemented. Where that affects safe use, the manual
identifies the current limitation instead of presenting the requirement as a
working guarantee.

`docs/reference/` is intentionally outside the published manual. It retains
research, audits, plans, and historical investigations. The published reference
layer is `docs/reference-manual/`, avoiding a collision with that preserved
material.

## Related top-level documents

- [README.md](../README.md): product overview and quick start.
- [SPEC.md](../SPEC.md): normative behavior and compatibility requirements.
- [AGENTS.md](../AGENTS.md): contributor-only repository workflow and validation rules.
