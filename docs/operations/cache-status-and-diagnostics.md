# Cache status and diagnostics

## Purpose

Interpret pane-level cache and context diagnostics without mistaking provider
reuse metrics for a correctness or authorization signal.

## Prerequisites

Open the affected pane's [agent shell](../using-mezzanine/agent-shell.md).

## Inspect status first

Use `/status` for the active pane's model, policy, writable roots, context
usage, and token information. At debug or trace logging levels,
`/copy-trace-log` exports the pane's bounded retained diagnostic trace. Review
that export before sharing it: it can contain task and action diagnostics, and
it is not a substitute for the redacted audit log.

`Cumulative cache hit` is a token-weighted ratio across retained provider
samples, including cold starts and auxiliary routing or sizing requests.
`Latest request cache hit` describes the most recent execution-model request.
A missing provider counter is `unknown`; an observed zero is `0.00%`.

## Understand normal cache changes

Compaction creates a new immutable request shape, so the following request can
be cold. Provider or model switches, explicit MCP state, and typed recovery
interactions can also change the request shape. A later warm request replaces
the latest sample but does not erase a prior cold sample from cumulative
accounting.

Trace continuity comparison reports the longest matching immutable prefix,
whether durable chronology grew append-only, estimates for immutable and live
state, and a classification such as `new_turn`, `compaction`,
`provider_switch`, `model_switch`, `append_only`, or `unexpected_rewrite`.
The last classification indicates a settled-context consistency signal, not a
provider cache decision.

`Stable provider prefix` compares only cache-eligible OpenAI input. It can
remain append-only even when complete-message continuity is false because a
request-local state message is regenerated after newly settled chronology.
Pane environment facts are frozen as typed prompt-boundary snapshots: an
unchanged environment adds no message, while a changed or unavailable
environment appends a new snapshot without rewriting the prior prefix.
Ordinary requests therefore do not repeat the frozen working directory in the
volatile suffix. A non-empty volatile byte count should come from an explicitly
request-local producer such as selected MCP metadata or recovery guidance.

`action_result_bytes` reports exact durable action-result content in the
observed request. Those bytes are cold when first appended, then remain in the
same chronological position for later requests and turns until compaction.
`stable_mcp_bytes` reports configured always-exposed MCP catalog snapshots in
append-only chronology. `explicit_mcp_bytes` reports request-local manifests
for integrations selected only with `@server`; those bytes can still move with
the volatile suffix. An unchanged always-exposed catalog should not increase
the snapshot count or create an MCP-caused stable-prefix divergence.

## Escalate a diagnostic safely

Check whether a compaction, model choice, project-guidance change, or requested
integration explains the result before treating it as a failure. Preserve the
bounded trace and relevant action result, then follow the provider, trust, or
terminal symptom owner rather than exposing raw context in a report.

## Related pages

- [Context and continuity](../agent/context-and-continuity.md)
- [Audit and diagnostics](../safety-and-trust/audit-and-diagnostics.md)
- [Troubleshooting](troubleshooting.md)

## Next step

Read [Troubleshooting](troubleshooting.md) when the status result corresponds
to an observable operational problem.
