# Cache status and diagnostics

## Purpose

Interpret cache and context diagnostics without mistaking provider reuse for
correctness, authorization, or a privacy guarantee.

## Prerequisites

Open the affected pane's [agent shell](../using-mezzanine/agent-shell.md).

## Inspect status first

Use `/status` for the active pane's model, policy, roots, context usage, and
token information. At debug or trace logging levels, `/copy-trace-log` exports
the pane's bounded retained diagnostic trace. Review it before sharing: task
and action diagnostics can be sensitive, and trace output is not the audit log.

Use `/status --project` for the invoking pane's eligible accounting project,
or `/status --all-projects` for registered projects (including zero usage),
historical IDs and unattributed remainder. Neither scope accepts a path. These
are STATIC accounting snapshots, while model/context/permission diagnostics
remain pane-scoped. Add `--extended` in either order for off-actor durable history.
All projects share one UTC query instant, consistent read snapshot and visible
1/7/30/60/90-day window set. Missing registry/project/storage is unavailable,
not verified zero. Pane reset does not erase session or durable expense.

All token tables separate Harness, Provider and Model. Native calls are labelled
`mez`; external observations retain their registered harness and category coverage.
Missing reasoning/cache counters are unknown, not zero. External-only status does
not create a native conversation, and external reports never replace native
latest-request/context samples. Pane reset leaves cumulative stream checkpoints,
runtime-instance expense and durable history intact.

| Observation | Interpretation |
| --- | --- |
| `Cumulative cache hit` | Token-weighted ratio across retained provider samples, including cold starts and auxiliary routing or sizing requests. |
| `Latest request cache hit` | Most recent execution-model request, not the complete turn. |
| `unknown` | The provider did not supply a usable counter; it is not an observed zero. |
| `0.00%` | An observed zero reuse ratio; it is not by itself a continuity defect. |
| `cache_write_input` | Provider-reported cache-write detail. For OpenAI Responses it is a subset of input, not extra tokens to add to the total. |

## Distinguish a cold request from a continuity failure

Compaction, a provider/model switch, or an exceptional recovery can change the
request shape and produce a cold request. A later warm request updates the
latest sample without erasing earlier cold samples from cumulative accounting.
Provider residency, load, and elapsed time can also affect reuse.

Trace classifications such as `new_turn`, `compaction`, `provider_switch`,
`model_switch`, and `append_only` help explain changes. `unexpected_rewrite`
is a local continuity warning, not the provider's cache decision. Preserve that
diagnostic and the preceding change when escalating; repeatedly retrying a task
does not prove or repair cache behavior.

For OpenAI, `Provider wire prefix` compares the ordered input and cache-affecting
request settings actually sent. `input_bytes` is the serialized input size;
`common_bytes` is the identical leading input size. `append_only=true` requires
that prior input remained an exact prefix and the relevant request envelope
was unchanged. This proves local request continuity, not that the provider
stored or reused those bytes.

ChatGPT browser/device diagnostics describe the request after adapter
transformation, including whether cache options survived, byte counts/digests,
requested control categories, and elapsed duration. Header and response
identifiers are represented by presence or digests rather than raw values in
these diagnostic fields. Missing metadata does not prove a routing failure or
cache miss. Do not generalize the redaction of these fields to the entire trace,
which can contain other task diagnostics.

Changes in action-result or MCP-directory bytes explain request growth; they do
not establish permission to call a connector. Forked conversations use distinct
cache routing keys. That separation does not guarantee cache hits, provider
eviction isolation, billing isolation, or a provider-side security boundary.

## Run an optional live OpenAI observation

The ordinary test suite does not call providers. This opt-in probe makes **two
live, potentially billable requests** to the canonical direct OpenAI Responses
API with a synthetic prefix, not project or conversation data. It requires an
API key supplied through the environment and an explicitly selected model.
Run it only on a trusted machine; environment secrets remain accessible to
appropriately privileged local processes. Do not type a real key into shell
history or enable shell tracing.

**Current credential-exposure limit:** the script also passes the authorization
header to `curl` as a command-line argument. The key can therefore be visible
to processes or monitoring tools allowed to inspect that process's arguments,
even though the probe's own output is sanitized. Do not run it on a shared host
or where process-argument collection is enabled without an approved credential
handling policy.

With `OPENAI_API_KEY` already supplied by your approved credential workflow:

```sh
MEZ_OPENAI_CACHE_PROBE=1 \
MEZ_OPENAI_CACHE_PROBE_MODEL=gpt-5.6 \
just probe-openai-prompt-cache
```

Select a model available to your account. The default mode is `implicit`;
`MEZ_OPENAI_CACHE_PROBE_MODE=explicit` is accepted only for a GPT-5.6-or-newer
model that supports explicit breakpoints. Each request has a default 60-second
timeout, configurable with `MEZ_OPENAI_CACHE_PROBE_TIMEOUT_SECONDS`.

The second request retains the first as an exact input prefix. The probe prints
sanitized shape/digest, timing, and usage observations, not the key, prompt, or
output. A zero or missing `cached_tokens` value is an observation, not a test
failure. The probe qualifies only the direct API-key backend; it does not
qualify ChatGPT browser/device caching. Do not substitute browser/device
credentials for an API key.

## Escalate safely

1. Record the provider/backend, model, observed counters, and timing.
2. Note recent compaction, model, project-guidance, or integration changes.
3. Preserve the bounded continuity diagnostic and relevant action result.
4. Review and redact a copy before sharing; never attach raw conversation
   context or credentials merely to explain a cache ratio.

## Related pages

- [Context and continuity](../agent/context-and-continuity.md)
- [Audit and diagnostics](../safety-and-trust/audit-and-diagnostics.md)
- [Troubleshooting](troubleshooting.md)

## Next step

Use [Troubleshooting](troubleshooting.md) when cache status accompanies an
observable execution, provider, or persistence problem.
