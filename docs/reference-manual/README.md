# Manual reference

## Purpose

Look up process commands, in-session commands, key bindings, agent actions,
and terminal behavior. Task-oriented chapters link here for syntax and limits.
The separate protocol subsection is intended for integration implementers;
users do not need it to run Mezzanine.

## Prerequisites

Use the task-oriented chapters elsewhere in the manual for guided workflows
before using this reference.

## Choose a reference

Use the page that matches the interface you are working with:

| Interface | Reference | Covers |
| --- | --- | --- |
| Process command line | [CLI reference](cli.md) | Invocation, session administration, snapshots, configuration, and remote targeting |
| Attached terminal | [Key bindings](key-bindings.md) | Default prefix controls, prompt input, and key presets |
| In-session `:` prompt | [Terminal commands](terminal-commands.md) | Command syntax, discovery, and the canonical command inventory |
| Agent work | [Agent actions](agent-actions.md) | Action names, execution results, approvals, and safe recovery |
| Pane terminal | [Terminal compatibility](terminal-compatibility.md) | Pane-facing profiles, supported modes, limitations, and diagnostics |
| Integrations | [Protocol reference](protocols/README.md) | Implementer summaries for [`mezctl/2` and `mezctl/3` JSON-RPC](protocols/control-json-rpc.md), [`maap/1`](protocols/maap.md), and [`mmp/1`](protocols/mmp.md) |

## Related pages

- [Using Mezzanine](../using-mezzanine/README.md)
- [Agent and integrations](../agent/README.md)
- [Operations and troubleshooting](../operations/README.md)

## Next step

For a guided workflow rather than an interface contract, return to [Using
Mezzanine](../using-mezzanine/README.md).
