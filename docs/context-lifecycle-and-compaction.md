# Context Lifecycle and Compaction

Mezzanine preserves one provider-independent context sequence. Chronology and
authorship are correctness contracts; cache reuse is optimized only inside
those contracts.

## Semantic model

Every block has separate placement, semantic, retention, provenance, and role
properties:

- `StablePrefix` contains invariant ambient instructions and stable
  configuration.
- `ConversationAppend` contains immutable task and conversation chronology.
- `EphemeralTail` contains only factual live state prepared for the next
  request and discarded afterward.

Semantic kinds distinguish ambient instructions, task preludes, direct user
events, assistant events, settled evidence, neutral references, and live state.
Retention distinguishes exact barriers, complete execution groups,
summarizable history, and request-local state. Providers may wrap neutral
context in a supported transport role, but the wrapper identifies it as
non-user-authored.

Validation rejects stable conversation events, append-only live state, tail
events, false user authorship, task preludes inserted after the active prompt,
duplicate active prompts, non-increasing sequences, unowned evidence, invalid
provider ownership, and lifecycle regressions. It reports the offending block
index, source, placement, semantic kind, retention, and rule. It never sorts
malformed input.

## Stored and prepared context

Durable `AgentContext` stores named stable slots and sequenced append-only
conversation events as distinct private types. Request-local state has a third
type and never enters durable storage. Adapter-facing blocks and metadata are
read-only projections rebuilt from typed storage. Checked mutations validate an
isolated candidate before commit, so an error cannot expose a partial chronology
or advance its event high-water mark. The initial sequence is built in explicit
stages:

1. stable authority and repository guidance;
2. compacted older history and retained raw transcript;
3. local messages that already arrived;
4. task environment and active skill/task prelude;
5. the exact user prompt, once.

Event identities are sparse and monotonic. A running-turn compaction refresh
may replace only the contiguous imported history prefix and allocate new history
identities before the first retained event. The prompt and every later event
keep their existing sequence and causal owner; insufficient sequence space is a
typed atomic failure, not permission to renumber chronology.

Memory and local/controller reference material remain neutral. Session id,
cache lineage, and similar diagnostic identity travel as typed request metadata
and never become model-visible blocks.

Provider preparation clones the durable sequence, builds a separate
`PreparedModelContext` live-state suffix, validates the joined view, sends it,
and discards the suffix. Repeating preparation does not mutate or grow durable
context.

During a turn, assistant action requests, settled results, controller results,
local or routed messages, and user steering are appended at their actual
occurrence boundaries. Steering preserves only the exact user text. The prompt
is the last event only when it is first committed; later assistant output and
evidence follow it. It is never repeated after its evidence and no adapter may
timestamp-sort events or relocate a transport-level user message to the end.

Accepted assistant output is projected once into provider-neutral causal text:
batch `rationale`, optional durable `thinking`, action-local rationale, raw
conversational text, and bounded action summaries in provider order. Raw MAAP
JSON and unbounded action payloads are omitted. Terminal presentation may hide
duplicate rationale, but it cannot rewrite the canonical projection. The same
projection is persisted byte-for-byte for restoration.

Every provider task claims the highest event sequence in the snapshot it
consumed. Steering and local messages commit at actor receipt. A response from
an older snapshot is discarded before its assistant output or proposed actions
become visible. If an action was already dispatched, its later result remains
after the steering event and retains its original causal owner; chronology is
never rewritten to make that owner contiguous.

## Request-local allowlist

Tail state is included only when it can change the correctness of the next
response and no typed provider field or durable event already carries it:

- authoritative current working directory;
- OpenAI interaction/action narrowing because its cache-stable action schema is
  intentionally a superset;
- the explicitly invoked OpenAI MCP manifest that its stable generic MCP action
  cannot express; and
- concise unavailable-MCP diagnostics when the requested integration affects
  the next response.

The tail excludes prompt or steering events, action results, transcript,
memory, skill instructions, readiness, write scopes/conflicts, scheduler state,
retry/recovery counters or mode names, session/pane/cache identity, environment
hashes, permission policy, progress/rationale ledgers, action pressure, generic
failure coaching, resolved-skill hints, and duplicate MCP schemas.

## Settled action evidence

Running actions and blocked approvals remain controller state. Once an action
reaches a deterministic terminal status, its canonical result is committed
exactly once to `ConversationAppend`:

- an unresolved batch is rejected atomically;
- stale request-local or legacy copies are removed;
- an existing identical result keeps its original position; and
- replay does not duplicate or reorder evidence.

Assistant output, provider-native tool events, and their settled results share
one causal execution owner derived from the accepted request/response identity.
Exact replay is idempotent, but equal response text from different consumed
request histories creates different owners. Every synthesized action id is
registered to its owner, and a result commits to that owner even if another
assistant response has since arrived. They remain complete and ordered during
persistence and compaction. An owner may straddle steering when
already-dispatched work settles later; that fact does not permit compaction to
gather records across the barrier.

Canonical storage keeps two projections when a provider requires native tool
continuity: the neutral assistant/action-result sequence and typed native
assistant/tool events. Assembly sends only the native projection back to its
owning provider and only the neutral projection after a provider switch.
Persistence orders the neutral assistant, native call/result records, and
generic results so compatibility restoration reconstructs one execution group
before choosing a projection.

Issue-query evidence has a logical-turn freshness key built from the normalized
project, kind, state, text, and limit. A repeated successful query is answered
by a structured skipped result pointing at the prior action result. Successful
issue mutations invalidate the keys; failed queries do not create them. The
explicit `refresh` flag bypasses reuse only for an externally changed store.

## Capability continuation across provider workers

Capability negotiation is logical-turn state, not provider-worker-local state.
When the controller accepts a capability request, it commits one neutral
controller-evidence event immediately before the assistant response that uses
the granted surface. That evidence is part of the same causal execution group;
it is neither a new user instruction nor a late task prelude.

If an action returns control to the runtime actor and the logical turn remains
running, the next provider dispatch inherits the last accepted request's
cumulative allowed-action set and interaction kind. The newly assembled request
therefore continues from the latest assistant action and settled result instead
of presenting the original prompt under a fresh capability-decision mode.
Actor-owned exceptional modes still override inherited state. Live MCP, memory,
and issue gates are reapplied before dispatch and revoke retained actions whose
backing integration is no longer available.

This continuation rule is scoped to one logical turn. It does not change how a
controller command chooses to create separate turns or conversations.

## Durable routed handoffs

The parent presentation turn stores one versioned routed-handoff reference event
immediately before the visible parent answer. The event contains the validated
bounded handoff summary. Exact worker output and presentation-only instructions
remain request-local. Malformed, unsupported, or ordinary system transcript
records remain filtered.

Routed handoff, repair, presentation, and failure explanation use typed
interaction modes. Static mode rules enter the system profile; dynamic output
or validation failures remain chronological evidence or bounded factual live
state. A mode change is an expected cache break, not a reason to reorder the
conversation.

## Barrier-aware compaction

Compaction runs on durable context before live state is attached. Exact,
non-crossable barriers include:

- the active user prompt and every steering event;
- active skill/task-prelude instructions;
- delegated or routed task statements required to interpret the work; and
- existing summary epochs.

The compactor freezes the provider-consumed event high-water mark, splits that
chronology at exact barriers, forms closed contiguous execution ranges inside
each segment, and selects the oldest eligible ranges. Events committed after the
consumed boundary remain raw. If one execution owner appears on both sides of a
barrier, both fragments remain raw. Each new summary replaces its original
contiguous range in place. It cannot move across a task/user barrier. Protected
blocks and existing epochs remain byte-for-byte stable. A configured recent raw
suffix is retained only in complete groups. An open group retains its exact
assistant rationale, thought, action rationale, native replay records, and
settled results; compaction cannot summarize only part of that causal record.

Each summary contains a semantic recovery index accounting for every replaced
record, including outcomes, errors, decisions, artifacts, unresolved
obligations, and an exact recovery route. Content that cannot be safely
recovered remains raw or produces typed unrecoverable overflow.

Repeated compaction can therefore produce:

```text
user prompt
summary of actions/results 1-4
user steering
summary of actions/results 5-8
latest raw action/result group
request-local live state
```

If protected exact content plus minimum required request state cannot fit the
provider window, recovery returns an explicit unrecoverable-context overflow.
It never truncates or summarizes direct user or active task instructions.

Terminal transcript persistence remains idempotent per conversation and turn.
Forked and routed conversations retain captured source high-water marks, so
later parent records cannot leak into isolated replay.
