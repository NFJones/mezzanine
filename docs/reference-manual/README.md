# Manual reference

## Purpose

Provide stable command, key, action, terminal, and protocol reference material
that task chapters can link to without duplicating tables and protocol details.
For normative or exhaustive contracts, follow the linked sections of `SPEC.md`.

## Prerequisites

Use the task-oriented chapters elsewhere in the manual for guided workflows
before using this reference.

## Chapters

This section owns the following reference material:

- [CLI reference](cli.md): process invocation, session administration,
  snapshots, configuration, and remote targeting.
- [Key bindings](key-bindings.md): default prefix controls, prompt input, and
  key presets.
- [Terminal commands](terminal-commands.md): the in-session `:` command
  language and canonical command inventory.
- [Agent actions](agent-actions.md): executable `maap/1` action families,
  results, approvals, and recovery boundaries.
- [Terminal compatibility](terminal-compatibility.md): pane-facing terminal
  profiles, supported modes, and diagnostic boundaries.
- [Protocol reference](protocols/README.md): implementer summaries for
  [`mezctl/2` and `mezctl/3` JSON-RPC](protocols/control-json-rpc.md),
  [`maap/1`](protocols/maap.md), and [`mmp/1`](protocols/mmp.md), with links to
  their normative contracts.

## Related pages

- [Using Mezzanine](../using-mezzanine/README.md)
- [Agent and integrations](../agent/README.md)
- [Operations and troubleshooting](../operations/README.md)

## Next step

Choose [CLI reference](cli.md) for scripting and session administration,
[Key bindings](key-bindings.md) for interactive controls, or [Protocol
reference](protocols/README.md) to build an integration. Use [Terminal
commands](terminal-commands.md) for commands entered through the in-session
command prompt.
