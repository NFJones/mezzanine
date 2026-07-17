# Context Lifecycle and Compaction

Mezzanine keeps model-visible context in three ordered lifecycle phases. This
ordering is a correctness contract as well as a prompt-cache optimization.

`StablePrefix` contains invariant product, provider, configuration, and project
guidance. `ConversationAppend` contains immutable transcript chronology,
assistant executions, settled action evidence, routed handoffs, and compaction
summary epochs. `EphemeralTail` contains controller state regenerated for the
next request, including pane readiness, scheduler state, pending approvals,
retry hints, capability eligibility, and the current user instruction.

Provider request assembly rejects a phase regression. Producers insert at the
boundary for their phase; they do not sort a malformed context after the fact.
Sorting could change assistant/tool chronology while hiding the owner that
introduced the invalid placement.

## Settled action evidence

Running actions and blocked approvals remain volatile runtime state. Once an
action reaches a deterministic terminal status, the runtime commits its
canonical result exactly once to `ConversationAppend`. The commit is atomic:

- a batch containing an unresolved result is rejected without changing context;
- stale volatile and legacy copies for the same action are removed;
- an existing identical immutable result keeps its original position; and
- replaying the settlement does not duplicate or reorder evidence.

This operation is shared by shell and patch results, MCP and network calls,
memory and issue actions, messages and skills, configuration changes,
approvals, failure correction, and joined subagent completion.

## Execution-group-safe compaction

Compaction operates only on immutable chronology. One provider execution group
contains assistant output, associated provider-native tool events, and all
terminal action results for that execution. Durable transcript entries use the
stable turn id as the same group boundary.

Only a closed prefix of complete groups may be summarized. The selected raw
suffix is rounded to a group boundary and every retained block or transcript
entry remains byte-for-byte identical and in the same order. Open durable
groups remain raw. Ephemeral controller state is neither counted toward the raw
suffix nor copied into a summary.

If the newest group fits the configured raw-tail budget, it remains exact. If
that group alone exceeds the recovery budget, the raw suffix may be empty so a
provider context-limit rejection can still be recovered without tearing the
group. Each completed compaction adds one immutable summary epoch. Existing
epochs remain byte-stable, and later settled evidence resumes append-only
growth after the intentional compaction continuity break.

Terminal transcript persistence is idempotent per conversation and turn. A
duplicate lifecycle finalization cannot append the same execution group or
advance the active replay high-water mark twice. Forked and routed
conversations continue to use their captured source high-water marks, so later
parent records cannot leak into isolated replay.
