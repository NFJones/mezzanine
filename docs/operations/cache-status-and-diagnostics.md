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

Provider token summaries also report `cache_write_input` when the provider
supplies a cache-write counter. OpenAI Responses reports
`cache_write_tokens` as a subset of its reported input total: Mezzanine keeps
that detail for observability without adding it again to `input` or `total`.
An omitted write counter remains `unknown`, while an observed zero is shown as
`0`.

## Understand normal cache changes

Compaction creates a new immutable request shape, so the following request can
be cold. Provider or model switches, explicit MCP state, and typed recovery
interactions can also change the request shape. A later warm request replaces
the latest sample but does not erase a prior cold sample from cumulative
accounting.

Trace continuity comparison reports the longest matching immutable prefix,
whether durable chronology grew append-only, and a classification such as
`new_turn`, `compaction`,
`provider_switch`, `model_switch`, `append_only`, or `unexpected_rewrite`.
The last classification indicates a settled-context consistency signal, not a
provider cache decision.

`Provider wire prefix` compares the complete ordered OpenAI input sent on the
wire, together with cache-affecting request-envelope components. Ordinary
requests in one conversation and cache epoch retain every previously sent input
item byte-for-byte and append newly settled chronology after it. The reported
`input_bytes` is the canonical serialized size of the effective input array
actually sent. `common_bytes` is the corresponding size of the identical
leading item array, while `envelope_unchanged` confirms that instructions,
response format, tools, tool choice, cache key, and request controls did not
change. `append_only=true` requires both conditions.
Pane environment facts are frozen as typed prompt-boundary snapshots: an
unchanged environment adds no message, while a changed or unavailable
environment appends a new snapshot without rewriting the prior prefix.
Ordinary requests therefore do not repeat the frozen working directory outside
durable chronology. Provider-wire diagnostics are the evidence for continuity:
they report canonical input size and items, the common leading items and bytes,
and whether the cache-affecting envelope remained unchanged.

`action_result_bytes` reports exact durable action-result content in the
observed request. Those bytes are cold when first appended, then remain in the
same chronological position for later requests and turns until compaction.
`mcp_directory_bytes` reports compact always-exposed MCP directory records in
append-only chronology. Search results, explicit references, retrieved server
contracts, and MCP action results are likewise durable action evidence. A
retrieved contract permits a later `mcp_call` only while it remains in the
current compaction epoch; live registry validation remains required. An
unchanged directory must not increase the snapshot count or create an
MCP-caused provider-prefix divergence.

OpenAI prompt-cache routing keys include prompt profile/version, provider,
lineage, session identity, typed workload purpose, a privacy-safe workload
partition, and cache-family identity. Forked sessions and forked subagents
retain their parent's lineage but receive distinct routing keys. Internal router
and structured-workflow traffic uses separate purposes, and diagnostics expose
only the purpose and partition digest. Session isolation does not guarantee
cache hits or provider eviction/billing isolation.

## Run an opt-in OpenAI cache conformance observation

The ordinary test suite never makes provider calls. To observe direct OpenAI
Responses behavior for a synthetic paired prefix, explicitly authorize the
probe and provide an API key only through the environment:

```sh
MEZ_OPENAI_CACHE_PROBE=1 OPENAI_API_KEY=... \
MEZ_OPENAI_CACHE_PROBE_MODEL=gpt-5.6 \
just probe-openai-prompt-cache
```

Set `MEZ_OPENAI_CACHE_PROBE_MODE=explicit` only for a GPT-5.6-or-newer model
that accepts explicit cache breakpoints; the default is `implicit`. The probe
sends two requests with the same synthetic stable prefix and distinct fixed
suffixes. Its second request retains the first request as an exact input prefix.
It prints only backend, model, cache generation/mode/TTL, request ordinal,
append-only status, purpose and partition digest, request-shape digest, token
counters, and whether a response id was present; it never prints the key,
prompt, output, endpoint, or credential. A zero or missing `cached_tokens`
value is an observation, not a defect: inspect continuity, elapsed time,
routing load, and provider residency before drawing conclusions. This probe
currently qualifies only the direct API-key Responses backend; the separate
ChatGPT browser/device backend remains explicitly unsupported until its cache
semantics can be tested without treating its credentials as REST API keys.

Changes to the model, provider routing namespace, prompt-cache lineage, stream
shape, compaction epoch, or an explicitly exceptional interaction start a new
cache epoch. Other changes to cache-affecting instructions, tools, tool choice,
response format, or request controls fail closed before an ordinary continuation
is sent. Operational controls - reasoning effort, service tier, and verbosity -
are excluded from cache identity, as are provider-native reasoning objects
(DeepSeek `thinking`, Anthropic `output_config.effort`) and sampling or output
caps (`temperature`, `stop`, `max_tokens`): the emitted body keeps carrying them,
but a change to only those never starts an epoch or reports a continuity
divergence by itself.

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
