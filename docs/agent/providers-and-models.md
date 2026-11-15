# Providers and models

## Purpose

Choose a provider and model profile, maintain a model catalog, and control
per-turn routing without assuming that configuration establishes entitlement.

## Prerequisites

Complete [authentication](../getting-started/authentication.md) and open the
active pane's agent shell. Keep credentials in `mez auth`, not configuration
or prompts.

Compaction freezes its model and provider request shape for the operation.
Genuine auxiliary input rejection repacks that source; transport retries keep
the failed request exactly. A usable final publishes without checking an
ordinary follow-up estimate. OpenAI requests omit `max_output_tokens` for
compaction and ordinary execution. See [context and continuity](context-and-continuity.md).

Capture selects that actual compactor projection on an owned copy, including
after a model switch; preparation leaves live history unchanged. An unexpected
live projection or frozen task-profile change refuses before credentials/model
dispatch. Automatic retries of an unchanged published source stop as context
unavailable; explicit manual compaction and fresh eligible history remain valid.

## Choose a provider

Mez has adapters for OpenAI Responses, Anthropic Messages, DeepSeek Chat
Completions, and custom OpenAI-compatible services. Authentication and the
provider connection are separate from model selection:

| Provider | Setup |
| --- | --- |
| OpenAI | `mez auth login` for interactive sign-in; explicit device-code and API-key methods are also available. |
| Anthropic | `mez auth login --provider anthropic --api-key` for an Anthropic Console API key. |
| DeepSeek | `mez auth login --provider deepseek --api-key` for an API key. |
| Custom OpenAI-compatible service | Configure its API dialect, base URL, models, and named profiles; store any required key with `mez auth login --provider <name> --api-key`. Custom login does not create those connection records. |

Successful built-in authentication adds that provider's defaults to the primary
TOML configuration without replacing an existing default selection. YAML and
JSON configurations need manual provider/profile setup. Check `mez auth status`
and `mez config validate` separately. For a custom service, see the
[configuration guide](../configuration/agents-providers-and-auth.md) or the
[Bedrock walkthrough](aws-bedrock-openai-compatible.md).

## Select a profile or model

A **model profile** is a named combination of provider, model, reasoning,
latency preferences, capability requirements, and non-secret options. Use a
configured profile to switch providers, then inspect that provider's catalog:

```text
/model
/model anthropic-default
/model list
```

The example requires the `anthropic-default` profile to be configured. Bare
`/model` reports the current selection and configured profiles; `/model list`
lists models for the **active provider**, not every provider. A bare model ID
or alias selects within that active provider. To choose a supported reasoning
level, use `/model <model-id> <reasoning-level>` or
`/model <model-id> --reasoning <reasoning-level>`.

The default selection scope is the current pane. `/model --clear` removes that
override rather than changing configuration. Other scopes are `session`,
`window`, `agent`, and `subagent`; use `--target` when selecting for another
identity. For a child:

```text
/model --scope subagent --target <agent-id> <profile-name>
/model --scope subagent --target <agent-id> --clear
```

Replace the placeholders with verified identities and configured profile names.
Overrides resolve in subagent, agent, pane, window, session, then default order.
Clearing a child's explicit override restores its spawn-selected profile.

Model selection does not establish entitlement or silently lower configured
safety, privacy, residency, or approval characteristics. If a model is
unavailable, inspect the error and configured fallback profiles instead of
assuming an equivalent replacement was selected.

If local request validation fails and failure reporting cannot safely settle,
the affected pane reports a provider failure settlement refusal. Mez retains
the original source and causal error, blocks further prompts and destructive
cleanup in that pane, and disables automatic retry. Other panes and control
connections remain available; the refusal does not establish that outstanding
effects or transcript writes completed. Preserve the diagnostics and retained
session while investigating the underlying validation or storage error. `/stop`
still requests cancellation of owned workers, then reports the refusal while
retaining their required results and cleanup obligations.
Results arriving later from MCP or network calls stay retained; Mez does not
retry those calls.

### Staged session-budget accounting

The native budget integration retains one original descriptor per concrete
exchange, including DeepSeek's internal structured-output retry. Its next
descriptor is allocated only after the previous usage receipt commits and the
actor adopts that observation. A refused next admission preserves the first
exchange's known usage without making another transport call. Missing counters
remain unknown, not zero. Receipt acknowledgment is not proof that issued
provider exposure has retired.

Memory and generated-title worker failures retain explicit cumulative incurred
usage independently of the failure outcome or title sanitization. If cumulative
usage is absent, reported output-cutoff usage remains the fallback; a last-call
cutoff never replaces an explicit aggregate. The original actor accounting owner
settles that expense once, including when its terminal event is delivered again.
This does not turn a rejected auxiliary result into success or authorize a retry.

Configured dispatch attaches the native source whenever its original budget
owner is present. Each exchange freezes its actual request provider/model and
code-owned purpose rather than borrowing the worker profile or response model.
Title, macro judgment, sandbox assessment and failure summary stay distinct in
the budget ledger; their diagnostic category remains `auxiliary`. Failure-summary
output/MAAP repair rounds keep the summary purpose. This connection does not
enable production membership, root-wide effect inhibition, live policy
publication or user controls; those remain staged.

Turn-less expense owners also retain their original compaction generation,
memory observation, or title dispatch and claimed attempt. They can allocate
concrete attempts only for that auxiliary purpose, and a held attempt cannot
match an ordinary owner or a different auxiliary task merely because the
conversation and root agree. Retaining this identity preserves original expense
custody; it is not proof that a retired task is live or permission to resume it.

Compaction's actual actor claim likewise owns its send fence. Publishing a newer
logical generation revokes older sends even before the successor is claimed;
Stop, completion and same-generation claim replacement revoke before removal.
Dispatch, publication and late-expense clones cannot revive that allocation.
The final native check follows binding validation without I/O or await before
permit consumption. A refused unused grant settles without HTTP, while an
already-admitted request keeps its original expense. This does not yet suspend
the actor's compaction lease by itself or establish fair auxiliary capacity release.

For activated memberships, the monitored compactor now retains the same model
wave through positive budget holds. The actual claim stores a bounded wait with
the original native owner, live allocation fence, positive hold epoch and weak
original-future lifetime. The actor replaces the active watchdog before
acknowledging parking, retaining the original remaining lease rather than a
fresh allowance. A paced exact-claim watchdog still retires dead or revoked
parked work. Resume preserves human Pause and installs that remaining lease
before fresh native authorization. Duplicate calls cannot extend it; stale
pre-park timer callbacks cannot expire a resumed lease early. This does not yet
establish fair auxiliary capacity release/requeue or production activation.
If the timer lane is full, the actual claim retains one bounded watchdog rearm
obligation. Capacity progress retries it without replaying the original event
or failing the shared timer service; the actual offer deducts queued active time.

Remember's monitored observer uses the task's original conversation, independently
of its runtime-scoped prompt-cache key. The actual actor claim owns its send
fence; dispatch and late-expense clones borrow it weakly. Stop, completion and
replacement revoke that fence before removing the claim. A final native check
after binding validation prevents an unissued retired request from sending,
settling any unused grant instead. Requests admitted before Stop still settle
their original known expense.

For activated memberships, monitored remember requests now keep that same future
through a positive budget hold. The actual claim keeps one
bounded wait, qualified by its original observation, native membership, positive
hold epoch and the waiting future's lifetime. Paced reads recheck it even while
policy remains exhausted. A current non-resumed hold is quiescent for human Pause;
lifting policy cannot clear human Pause or revive Stop. Actor resume precedes
fresh native authorization. The shared clock records acknowledged parking, but
remember has no cumulative active-allowance consumer yet; existing per-exchange
HTTP deadlines begin at transport execution. Connecting a finite original memory
allowance without resetting it across parks remains unfinished, as does fair
auxiliary capacity release/requeue.

The runtime also has a staged retained-claim parking port. It accepts only a
sealed capture of an observed nonwriting hold, releases scheduler capacity while
keeping the original claim and conversation live, and fairly reacquires without
offering another provider continuation. Reconsideration revokes old captures.
The caller must adopt original claim-timer cancellation/rearming before resuming
that same inference future and obtaining fresh authorization. Monitored ordinary
workers now connect this path for already activated memberships: they retain the
original future and finite active-time allowance, excluding only acknowledged
parking. Read-only checks run at 100ms to 2s intervals. Relevant changes permit
fair reacquisition; a fresh capacity refusal can also be reconsidered when a
paced read shows a free global slot, even if counts cycled back to an earlier
value. These observations never reserve capacity or grant send authority.
Queue pressure and sibling startup failure retain the waiter and its original
timer obligation. Stop still retires the original producer/journal without a
send. A still-live positive nonwriting budget hold is quiescent for human Pause,
while independent issued effects and persistence still keep it Pausing. Human
Resume clears only that human gate: the original frozen request, claim and budget
wait survive, without replacement guidance or another provider continuation.
Revoked or active holds cannot supply this nonwriting proof.

Monitored generated-title requests also retain their original future on a
positive hold. The existing title claim holds one bounded wait record, qualified
by the exact conversation, dispatch, claim attempt and current native membership.
Each paced check revalidates the actual publication claim, not the separate
late-expense owner. Only a live waiting future can exclude its parked interval
from the title lease; duplicate park/resume cannot extend it twice. Human Pause
stays independent, and closing/replacing the original claim cannot authorize a
send after policy release. Actor adoption restores the remaining lease before
fresh native authorization, never supplies that authorization itself.
The final atomic claim check follows native binding validation immediately
before permit consumption. Retirement wins if it precedes that check, including
when authorization finishes after actor resume; the unused original grant is
withdrawn and settled without HTTP. Admission ordered before retirement remains
issued exposure and is not implicitly rolled back.

Human `/stop` also retires the invoking conversation's queued or claimed title,
including a parked title without a live ordinary turn. Original expense custody
remains available for late settlement; a later policy release cannot revive it.

The title connection still retains the existing optional-title concurrency
slot. Neither title nor memory parking implements auxiliary fair requeue,
compaction waits, root-wide effect gates, or production membership activation.

Native async sends also retain evidence that their exact local transport future
and active callback futures were destroyed, including on cancellation. This transfer
precedes normal receipt capture; it does not establish remote completion,
complete counters, detached-worker retirement, or durable exposure release.
Destructor failure cannot create that evidence; consumer failure fences the
original journal while preserving its expense. No storage work runs from Drop.

The staged native driver commits known subtotals first, then a separate immutable
terminal-unknown receipt together with an original-grant retirement reference.
This conserves expense without treating partial counters as complete coverage.
Recognized final unary reports carry separate original-subject/value/presence
evidence before output parsing. Explicit terminal Responses statuses or finished
chat choices qualify only positively reported primary dimensions, including true
zero. Cache/reasoning-only reports retain their lower bounds without completing
missing dimensions; stream prefixes and nonterminal bodies stay incomplete.
Malformed MAAP cannot erase already qualified expense or final counters. Neither
final-counter evidence nor transport retirement alone replaces the other.
Responses SSE likewise retains the explicit terminal event's own counters and
presence, separately from accumulated prefixes. Missing final fields stay
incomplete even when earlier frames reported them, and `[DONE]` alone does not
complete coverage. Conflicting final reports lose completeness; regressing final
totals preserve the prior prefix and refuse inconsistent durable release. Failed
or incomplete output remains unusable despite its retained expense. Interrupted
or HTTP-refused streams conservatively retain incomplete coverage.
Once a terminal regression is observed, its refusal remains retained even across
conflicting repeats or callback cancellation; it cannot become ordinary unknown
coverage that frees the original exposure.
OpenAI-compatible and DeepSeek Chat Completions streams separately qualify the
usage-bearing chunk after all observed choices finish. Only that chunk's own
fields complete dimensions; `[DONE]` and accumulated presence are insufficient.
Conflicting final reports invalidate output and completeness; terminal regression
retains the original expense and refusal. Malformed/excessive choice metadata
keeps coverage incomplete.
Anthropic uses separate Messages grammar: an assistant unary message with a stop
reason, or a message-start plus stop-reason delta and `message_stop` for SSE.
Inclusive input is complete only with all three explicit checked components:
uncached input, cache read and cache creation. Missing cache fields stay unknown,
not zero. SSE final output comes only from the terminal delta, never start or
prefix presence. A complete delta input report can replace the start report;
changed partial input makes that dimension incomplete while preserving final
output. Contradictory terminal reports invalidate output and completeness, and
component regression retains expense plus sticky refusal even on cancellation.
Simultaneously higher primary counters remain retained; this does not add cache
component maxima together to invent a new inclusive total.
All qualifications require successful HTTP completion; malformed MAAP can still
retain qualified usage without authorizing its output.
Only that qualified durable transfer frees local exposure; the original grant
remains replay-only forever. Receipt or actor acknowledgment alone cannot release
it. Older grants migrate as outstanding, even if old terminal expense exists.

If cancellation occurs during authorization, the driver first joins its original
worker. Only a still-retained, never-consumed original permit can be withdrawn
into non-send custody and settled through the same two durable stages. A stored
grant or a record of an already handed-out permit cannot substitute for that
custody. A failed terminal write retains the original withdrawn owner and fences journal
acknowledgment; retries require actual writer observation and positive absence.

These internal allocation and accounting ports do **not** yet enable operational
session budgets. Production assembly, fair waiting, complete terminal coverage,
physical retirement, root-wide activity inhibition, policy controls, and their
connected acceptance remain required; the ports alone are not a spending cap.

## Maintain the catalog

Configured model records under `providers.<name>.models` hold reusable IDs,
aliases, limits, reasoning levels, capabilities, and options. Profiles can
override them. Metadata precedence is profile override, configured model,
provider discovery, built-in metadata, then fallback. Omitted lists inherit;
explicit empty lists clear lower-precedence values. Option maps merge per key.
`reasoning_levels` lists supported choices; `reasoning_profile` selects one.

Choose the operation based on whether you need a temporary refresh or a durable
configuration change:

| Operation | Effect |
| --- | --- |
| `/refresh-provider-info` | Refreshes the running session's best-effort provider catalog and quota information; does not edit configuration. |
| `mez config model list PROVIDER` | Lists configured base model records. |
| `mez config model sync PROVIDER` | Previews a comparison with the raw live catalog. |
| `mez config model sync PROVIDER --apply` | Persists the validated sync plan. |
| `mez config model add PROVIDER MODEL_ID` | Adds an exact provider-facing ID when discovery is unavailable or incomplete. |
| `mez config model update PROVIDER MODEL_ID ...` | Selectively updates known metadata. |
| `mez config model remove PROVIDER MODEL_ID` | Removes an unreferenced record. |

Sync fills omitted metadata only. Explicit values, including empty lists,
remain authoritative and disagreements are reported as conflicts. Without
`--prune`, records absent from one provider response remain configured.
`--prune` proposes removals but still requires `--apply` to write; references
to affected IDs or aliases block the whole prune write. Review the preview
before applying it.

Model IDs are opaque values, even when they contain dots, slashes, or colons.
The typed commands generate safe local keys; do not splice an ID into a dotted
configuration path. Renaming or removing an ID requires updating matching
provider-default and profile references first. Use `--help` for metadata and
clear flags. These commands accept `--scope user|project` and `--file` targets.

The LM Studio example starts with an empty model table. Preview with
`mez config model sync lmstudio` and apply only verified discoveries, or add
the backend's exact ID manually. Follow the
[LM Studio setup](../configuration/reference.md#providersname) to create and
select a profile for that provider before using `/model list`, which lists only
the active provider. `/refresh-provider-info` refreshes all configured providers
without selecting one or persisting model records. Mez does not
import OpenAI built-ins into an empty custom-provider response or infer token
limits from a model name.

Configured models remain selectable when discovery omits them; unavailable
live metadata can fall back to configured records, with its source labeled.
Reload configuration after durable catalog changes, or start a new session.
An in-flight turn keeps its selected profile; a refresh affects future lookups.

## Control routing and thinking

Automatic sizing chooses an ephemeral small, medium, or large profile and
reasoning effort from workload scope and risk, not prompt length alone:

- `subagent` routing runs root work in one managed worker, then the parent's
  normal profile presents the result.
- `in-place` routing applies the selected profile to the current root turn.
- Already-spawned subagents route in place. A routed `/loop` pins its selected
  worker profile across iterations.

After the turn, the pane's ordinary selection remains unchanged. Routing may
therefore use a different execution model than the one shown by `/model`.

```text
/routing status
/routing on
/routing policy in-place
```

**Bare `/routing` toggles routing; it is not a read-only status command.** Use
`/routing off` to disable it. `/routing policy subagent` selects managed-worker
routing; `/routing policy --global in-place` persists a fallback policy for
panes without an override. `/model --routing` inspects the router model profile,
not the ordinary work model.

Use `/latency` to inspect or select a pane-local latency/cost preference and
`/thinking` to inspect native thinking on models that support it. Use
`/thinking on`, `/thinking off`, or `/thinking toggle` to change it.
Reasoning levels and thinking support are provider-specific: use current
metadata rather than copying another provider's values. Unknown DeepSeek
models disable native thinking, reasoning controls, and streaming until
metadata establishes support. A catalog refresh is not a remedy for missing
credentials, account entitlement, or quota.

## Related pages

- [Authenticate a provider](../getting-started/authentication.md)
- [Configure AWS Bedrock through its OpenAI-compatible API](aws-bedrock-openai-compatible.md)
- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Normative provider selection contract](../../SPEC.md#23-provider-model-selection)

## Next step

Read [MCP integration](mcp-integration.md) when the task needs an external tool
server.
