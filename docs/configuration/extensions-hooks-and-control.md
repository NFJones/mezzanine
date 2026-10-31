# Extensions, hooks, and control

## Purpose

Configure MCP servers, hooks, control access, audit records, and extension data
while keeping external capabilities visible and reviewable.

## Prerequisites

Read [Configuration overview](overview.md) and review the safety implications
of every external command, endpoint, credential reference, and hook.

## Configure integrations explicitly

`mcp_servers` contains configured Model Context Protocol integrations. Per-server
settings cover transport, enablement, tools, timeouts, and non-secret metadata.
Use `mez mcp` and `/list-mcp` to manage and inspect runtime state. Store MCP
secrets through authentication flows or environment references, not ordinary
configuration.

`hooks` configures lifecycle and command hooks. Hooks can execute or contact
external systems, so they remain subject to configuration trust, permission,
and audit requirements. The local control endpoint, agent messaging, and
snapshot storage are runtime-owned rather than configuration tables; use their
CLI and reference documentation to inspect those facilities. `audit` controls
structured security records.

Native filesystem attempt admission checks `pre_shell_command` hooks using
semantic action metadata, without generating a shell command. Required handlers
and handlers with a blocking failure policy reject admission before dispatch;
optional warn-only handlers produce an incompatibility diagnostic and do not
queue a program or focused-shell command.

Filesystem completion captures the original `post_shell_command` definitions
when the action is queued. Matching uses semantic action identity, action status,
effect certainty and confirmed endpoint/operation counts; it does not fabricate
shell commands or exit codes. Completion incompatibilities are diagnosed once,
even if required, while committed effects remain recorded. Later configuration
or pane-mode changes cannot enable executable completion handlers. Diagnostic
failures retain the first error and do not replay earlier diagnostics; a
replacement conversation receives no hook status paint. This hook milestone
does not acknowledge audit, publication, archive or worker ownership transfer.

Native filesystem actions retain their original audit writer registration and
policy when queued. Required disabled logging blocks admission, and commit
leases recheck the original writer. Replacing it, even with equal settings,
cannot waive or acknowledge a pending original record. Audit records describe
native filesystem effects and original action/conversation identity without
inventing shell commands or output. A failed staged append retains its state and
queued bytes through errors and unwinding and leaves the stream unproven for
native use. Failed or lost append acknowledgements retain the first error and
are not replayed; committed file effects remain recorded. An accepted deferred
append means that bytes are queued, not persisted. Physical persistence,
publication, archive and producer ownership transfer still need their own
evidence. Optional absent/disabled logging is explicitly not required.

Deferred native audit records carry an opaque physical receipt alongside the
original bytes, destination and retention policy in the ordered audit queue.
The worker retains its actual blocking handle across cancelled observers; a
later observer joins the same job. Physical acceptance requires the original
qualified I/O result and that actual join. Duplicate deliveries read the retained
result and never retry an entered write, including an uncertain failed prefix.
Path/byte notifications or another record's receipt cannot stand in for this
proof. A lost notification can be reconciled from the original retained receipt;
late positive evidence preserves an earlier append acknowledgement failure.
This proves the qualified append and retention operation, not permanent record
retention or publication/archive/producer transfer.

Treat hook runners as distinct execution boundaries. Program hooks can invoke
external programs and receive structured event data on standard input. Shell
hooks use the focused pane shell when one is available. Focused-shell hooks
marked `agent_hook` wait for shell availability; they do not run through the
agent action path. A configured hook is enabled by default and has a 30-second
timeout unless overridden. Use `program` plus `args` for a program hook, or
`command` with `kind = "focused_shell"` for a pane-shell hook. The accepted
`shell`, `env`, `cwd`, `working_directory`, `inject_instructions`,
`mutates_policy`, and `alters_action` fields are reserved and not consumed by
the current runtime; do not rely on them to select an interpreter, inject an
environment, change directory, or rewrite an action. `on_failure` may be
`block`, `warn`, or `ignore`, with
event-dependent defaults documented in the reference. A blocking failure stops
an operation that has not completed; after the triggering event has completed,
the same failure is reported as a warning. Inspect hook failures with
`show-messages` and audit records rather than assuming an event completed.

Native basic-action paths do not execute or queue program/focused-shell hooks
for prompt, turn, permission, semantic patch or MCP events. A required or
blocking pre-action handler blocks explicitly as incompatible, including a
required handler configured with warn/ignore. Optional and completion handlers
report that no handler ran; completed effects are not rolled back. Hooks for
actual shell commands and pane-mode actions retain their existing behavior.
Legacy patch shell-event payloads keep `action_type = shell_command` and add
`semantic_action_type = apply_patch`; runtime ownership determines admission.
No configuration field or event name changed.

Pane creation, external editors, clipboard/status commands and session/UI
lifecycle hooks are intentional process integrations outside the filesystem
guarantee, not hidden exceptions that a native patch may use as helpers. This
does not promise a globally process-free terminal multiplexer.

Synchronous program-hook output capture is cancellation-aware: a timed-out hook
does not wait for pipe EOF from escaped descendants. After normal child exit,
both output readers share a bounded drain grace period. If that drain cannot
complete, the captured prefix is marked truncated. This does not claim that
process-group termination can kill a descendant that deliberately escaped it.

Use `extensions` only for implementation-specific extension data. Unknown
top-level keys are rejected rather than silently interpreted as configuration.

## Related pages

- [MCP integration](../agent/mcp-integration.md)
- [Audit and diagnostics](../safety-and-trust/audit-and-diagnostics.md)
- [Configuration reference](reference.md)

## Next step

Return to [Configuration overview](overview.md) or validate the final file with
`mez config validate`.
