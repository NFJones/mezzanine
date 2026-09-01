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
