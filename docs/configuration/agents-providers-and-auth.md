# Agents, providers, and authentication

## Purpose

Configure agent defaults, provider definitions, model profiles, and
authentication boundaries.

## Prerequisites

Read [Configuration overview](overview.md) and complete provider sign-in with
`mez auth` before expecting model-backed turns to work.

## Configure agent and model behavior

The `agents` table controls the default provider and model profile, routing,
compaction retention, concurrency, loop limits, and subagent limits. Provider
definitions belong under `providers.<name>`. Reusable model facts belong under
`providers.<name>.models.<entry>`, whose required `id` is the canonical
provider-facing model id. Named `model_profiles.<name>` combine that base model
with reasoning, latency, capability, policy, and non-secret option overrides.
Use `mez config model list|add|update|remove` to manage base records by opaque
provider-facing id; Mez generates deterministic local entry keys and refuses
to remove or rename ids still referenced by `default_model` or a model profile.
Use `/model`, `/routing`, and `/thinking` for pane-scoped runtime choices where
supported.

`agents.compaction_mode` accepts `summary` (the default) or `raw_tail`.
Raw-tail compaction intentionally discards optional old input from the active
model context without generating a summary or calling a compactor model. It
preserves the archive, exact active instructions and open dependencies. It needs
no compactor credentials and incurs no auxiliary model usage. Changing the
setting affects new admissions; saved membership governs later replay and forks.

`/compaction-mode summary|raw_tail` overrides only the live invoking pane;
`/compaction-mode reset` resumes configured inheritance. Append `--global` to
persist a default or remove its primary-file key, preserving other config layers
and all explicit pane overrides. `/compaction-mode status` distinguishes the
effective choice from the primary value and reports pending/durable/failed writes.
Pane overrides are not saved or copied into new/child panes. Use `/compact` to
execute the selected policy, not to change it.

`agents.compaction_raw_retention_percent` selects the same complete raw tail
for manual, observed-input and provider-input-rejection compaction. Private
input packing and transport retries preserve that reservation. Advisory summary
length does not change it or create a provider generation cap. See
[context and continuity](../agent/context-and-continuity.md).

In raw mode, the percentage bounds one newest optional suffix after fixed
request overhead and protected source reserve capacity. Whole groups stay
together, and a newer oversized group prevents older backfill. A fitting,
unchanged tail is no-work; protected input that exceeds capacity is irreducible.
Actual provider input rejection can remove additional whole optional groups
until none remain. It never switches automatically to summary mode.

Provider outages use actor-owned exponential backoff. By default, Mez retries
eligible transport failures, rate limits, retry hints, 5xx responses, and
temporary provider unavailability five times. Configure the finite count with
`agents.provider_error_retry_limit`; `0` disables those finite retries. Delays
start at one second and grow to a maximum of 15 minutes.

Inference HTTP exchanges do not follow redirects or retry inside the HTTP
library. A redirect is returned to the provider adapter as a non-success response;
it cannot silently submit a second billable request. Explicit runtime/adapter
retries retain their own lifecycle. Ordinary fetch and search routing is unchanged.
Known decoded usage remains incurred even when later MAAP validation rejects the
provider's output. Async chat adapters capture recognized unary counters before
HTTP-status, envelope, MAAP and finish-state validation, including JSON error
bodies returned to streaming requests. Adapter-internal retries retain each
exchange separately, never charging the first request as the second's expense.
Missing or invalid counters remain unavailable; a recognized explicit zero is
retained, without claiming independent dimension completeness.
Native observations also retain primary input/output source presence separately.
A numeric zero is present; absent/null/invalid fields remain unknown even when
legacy normalized totals display zero. Cache or reasoning subsets alone do not
establish either primary counter. Cumulative omissions retain earlier evidence.
These bits survive original-owner cancellation custody but do not prove final
usage, worker retirement, durable acknowledgment or budget permission. Anthropic
primary input is cache-exclusive, so its presence alone cannot prove complete
inclusive input without separately qualified cache components.
The staged native budget repository also distinguishes attempt registration
from a one-use authorization. Its transaction checks current policy, root usage,
native binding and outstanding exposure together; a lost authorization reply
can recover evidence, not another send permit. Terminal usage alone cannot
release exposure. These internal ports do not enable production token budgets:
physical-send, fair-waiting and worker-retirement integration remains required.
All journal-owned provider purposes wait for their own observation's actor ACK
before returning or making an adapter-internal next exchange, including turn-less
requests. Losing that consumer fails locally and keeps the original evidence for
retirement; an original provider error and known expense remain intact. An actor
ACK is only handoff, not a durable budget receipt, send permit or retirement proof.
The staged durable-receipt owner retains the actual blocking writer across
canceled waiters. Frozen receipt clones share its original result. An uncertain
write requires observing that writer and querying the original receipt; only a
new positive absence query permits an explicit bookkeeping retry. Failed queries
never count as acknowledgment or absence, and no provider request is replayed.
This internal custody port does not enable production budget enforcement.
Budgeted observations retain that frozen receipt in the original wire journal
before channel delivery. The staged handoff commits it before actor adoption;
actor ACK alone cannot discard an uncommitted or conflicting receipt. One
registered attempt cannot be reused for another wire exchange, even through a
cloned descriptor. Wire receipts currently checkpoint subtotals only, preserving
missing/zero distinctions and qualified subset lower bounds without claiming
final coverage, releasing exposure, or duplicating rolling usage reports.
Failed or cancelled final flushing keeps the original journal and shared writer
in the existing service-retirement owner, along with its persistence dependency.
An explicit later bookkeeping recovery may complete the handoff, but does not
erase the original failure or replay inference/effects. Producer destruction is
not proof that a physical provider exchange has retired.
Chat adapters also retain already decoded counters when the
stream ends in a transport, HTTP-status, decoder, or finalization failure. Missing
usage remains unavailable; normalized counters alone do not prove full coverage.
When one streaming event carries both a provider error and usage, recognized
counters are retained before rejecting the output; the error remains a failure.
After the first streaming output error, subsequently received events still reach
the decoder for usage retention, but emit no further progress and cannot turn
that failed request into a successful result. This does not extend the transport
deadline or start another exchange to seek missing usage.
Cumulative stream frames replace only counters they actually report, including
explicit zero. Missing fields preserve prior subtotals; cache-exclusive input
components are retained independently before inclusive input is computed. A
final aggregate is not added a second time to its previously reported prefixes.
Observed streaming requests also transfer decoded expense to their original
retirement owner if the request future is cancelled or unwinds. This transfer
does not duplicate a normal observation already retained for delivery, and does
not prove remote completion or a durable budget receipt acknowledgment. Numeric
chat counters alone still cannot distinguish a zero-only report from no report.

Set `agents.provider_error_retry_unlimited = true` only for daemonized work that
should wait through an extended provider outage. This separate switch ignores
the finite count for eligible transient provider failures and continues at the
15-minute cap. It does not retry authentication, malformed-request, context-
limit, output-limit, or other non-retryable failures indefinitely. `/stop`,
session cancellation, and runtime shutdown still terminate retry work.

OpenAI Responses with an authoritative incomplete reason of `max_messages`
retry the exact failed inference, not earlier actions or negotiation steps.
These replacement retries use the finite provider-error count and backoff and
also consume the original turn-wide provider-call bound. Unlimited outage mode
does not extend them. Incomplete text and native calls are discarded: only a
complete replacement can produce executable actions. Reported token expense
is retained once, including when cancellation occurs during retry backoff.
Unknown incomplete reasons do not acquire retry eligibility from a failure
summary, and context/output limits retain their distinct recovery paths.

An input-context rejection can recommend reducing `max_tokens` or
`max_output_tokens` without being an output cutoff. When no structured category
is supplied, a recognized input-overflow diagnosis in the same message field
keeps bounded model-backed context compaction available; the parameter mention
alone does not select partial-output recovery. Explicit structured categories
still take precedence, and genuine output-budget validation remains separate
from input overflow.
Recovery does not manufacture partial output or replay settled actions.

Profile fields override configured model fields; configured model fields
override provider discovery; discovery fills gaps ahead of built-in and
conservative fallbacks. Lists replace lower-precedence lists, while option maps
merge per key from provider root through model and profile. Aliases resolve to
the canonical model id, but profiles may still name an unlisted custom model.
For the built-in DeepSeek V4.1 Flash record and the retained DeepSeek V4 Pro
tier, generated and code-defined fallback metadata declares reasoning levels
`low`, `high`, and `max` together with `native_thinking`, `function_tools`,
`forced_tool_choice`, `streaming`, and `max_output_tokens`. Omitted list fields
inherit that metadata; explicit empty lists clear it.
Refreshing provider information updates future resolutions without changing an
in-flight turn; config reload rebases retained generated profiles.

Provider configuration describes connections and model catalogs; it does not
store authentication secrets. Use `mez auth login`, `mez auth status`, and
`mez auth logout` for credentials and account state. Do not put API keys,
tokens, or bearer credentials in `config.toml`.

Search credentials have their own `mez auth search login|status|logout`
namespace and do not change model authentication or activate paid search. The
local foundation accepts future `search-auth:<name>` and explicit
`env:<VARIABLE>` selectors, not literal keys in configuration. Future environment
resolution uses the primary service's inherited environment: exporting a key in
a pane or a later client does not update a running daemon. Change the service
launch environment and restart/relaunch it to change that source; an already
resolved request keeps its own key snapshot for retries. Runtime search-profile
wiring is separate. See [Search credentials](../getting-started/authentication.md#store-a-search-credential-locally)
for local storage and environment lifetime.

## Configure identities and long-lived assistance

`subagents` defines named profiles and their narrower defaults. `personalities`
defines user-selected response styles. `memory` and `issues` configure the
local persistent stores used by the agent surfaces. Project overlays can add
allowed project-specific settings only after the project is trusted.

## Related pages

- [Providers and models](../agent/providers-and-models.md)
- [Configure AWS Bedrock through its OpenAI-compatible API](../agent/aws-bedrock-openai-compatible.md)
- [Authenticate a provider](../getting-started/authentication.md)
- [MCP integration](../agent/mcp-integration.md)
- [Configuration reference](reference.md)

## Next step

Read [Permissions, sandbox, and trust](permissions-sandbox-and-trust.md) before
granting a configured agent authority.
