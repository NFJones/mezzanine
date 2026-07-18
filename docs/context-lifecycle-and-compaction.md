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
and lifecycle regressions. It reports the offending block index, source,
placement, semantic kind, retention, and rule. It never sorts malformed input.

## Stored and prepared context

Durable `AgentContext` stores only stable and append-only blocks. The initial
sequence is built in explicit stages:

1. stable authority and repository guidance;
2. compacted older history and retained raw transcript;
3. local messages that already arrived;
4. task environment and active skill/task prelude;
5. the exact user prompt, once.

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
is never repeated after its evidence and no adapter may relocate a later
transport-level user message to the end.

## Request-local allowlist

Tail state is included only when it can change the correctness of the next
response and no typed provider field or durable event already carries it:

- abnormal pane readiness;
- authoritative current working directory;
- relevant write conflicts;
- compact scheduler counts and agent identities needed for immediate
  coordination;
- OpenAI action-superset narrowing;
- provider-required MCP manifest or unavailable-integration diagnostics; and
- typed recovery-mode identity with bounded factual evidence.

The tail excludes prompt or steering events, action results, transcript,
memory, skill instructions, full scheduler inventories, session/pane/cache
identity, environment hashes, permission policy, progress/rationale ledgers,
action pressure, generic failure coaching, resolved-skill hints, and duplicate
MCP schemas.

## Settled action evidence

Running actions and blocked approvals remain controller state. Once an action
reaches a deterministic terminal status, its canonical result is committed
exactly once to `ConversationAppend`:

- an unresolved batch is rejected atomically;
- stale request-local or legacy copies are removed;
- an existing identical result keeps its original position; and
- replay does not duplicate or reorder evidence.

Assistant output, provider-native tool events, and their settled results form
one execution group. They remain complete and ordered during persistence and
compaction.

## Durable routed handoffs

The parent presentation turn stores one versioned routed-handoff evidence event
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

The compactor splits chronology at these barriers, forms closed execution
groups inside each segment, and selects the oldest eligible groups. Each new
summary replaces its original contiguous range in place. It cannot move across
a task/user barrier. Protected blocks and existing epochs remain byte-for-byte
stable. A configured recent raw suffix is retained only in complete groups.

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
