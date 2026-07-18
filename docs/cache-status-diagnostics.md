# Cache Status and Continuity Diagnostics

Mezzanine exposes cache and context behavior without adding diagnostic text to
the model request. Use `/status` for pane-level summaries and
`/copy-trace-log` at debug or trace level for request detail.

## Cache reuse metrics

`Cumulative cache hit` is the token-weighted ratio across retained provider
samples. It includes cold starts and auxiliary router/model-sizing requests.
`Latest request cache hit` describes the latest concrete execution-model
request only. A missing provider counter is `unknown`; an explicit zero is
`0.00%`.

Compaction creates a new immutable request shape and can make the next request
cold. A later warm request replaces only the latest sample; it does not erase
the cold request from cumulative accounting.

## Context contribution diagnostics

For every canonical block, trace diagnostics expose only metadata and digests:

- index, placement, semantic kind, and retention;
- canonical role and provider-projected role;
- source and stable block-identity SHA-256;
- byte and approximate token counts;
- reusable-prefix participation; and
- request-local status.

Aggregate fields report stable-prefix, append-only, and live-state sizes; counts
and sizes by semantic category; the first volatile block; exact and normalized
near-duplicate counts; immutable and provider-projection hashes; and the
expected cache-break reason for typed exceptional modes.

The continuity digest includes placement, source, semantic kind, retention,
canonical role, label, and content. A change in authorship or protection policy
therefore cannot masquerade as identical context. Prompt and transcript text is
not retained in diagnostic state.

## Continuity classification

Request comparison reports:

- the longest exact immutable block prefix;
- whether durable chronology grew append-only;
- immutable and volatile byte/token estimates; and
- `new_turn`, `compaction`, `provider_switch`, `model_switch`, `append_only`, or
  `unexpected_rewrite`.

`unexpected_rewrite` means settled immutable chronology changed without an
explaining compaction or provider/model transition. It is a correctness signal,
not proof of a provider-side cache decision.

Typed interactions such as capability continuation, MAAP repair, output-limit
retry, failure summary, routed handoff/repair/presentation/failure explanation,
auto-sizing, and macro judging intentionally select different instruction
profiles. Diagnostics label their interaction kind as the expected cache-break
reason.

## Provider request shape

OpenAI traces separately fingerprint front-loaded instructions, response/tool
schemas, stable input, volatile input, the complete cacheable prefix, and the
provider projection. The compact OpenAI request-state narrowing block is the
first volatile contribution when no earlier live state exists.

Anthropic cache breakpoints close after the latest immutable message and before
neutral live state. OpenAI Chat, DeepSeek, and Claude Code lack the same
explicit breakpoint controls, but their provider-projection digest and ordered
per-block roles still expose a late insertion, false user projection, or
duplicate context block.

Changing only CWD, readiness, scheduler state, write conflicts, recovery state,
or required MCP live state should preserve the stable and append-only prefix.
No-op preparation should produce identical durable and provider-shape hashes.
