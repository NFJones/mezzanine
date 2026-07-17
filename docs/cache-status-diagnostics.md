# Cache Status and Continuity Diagnostics

Mezzanine exposes prompt-cache behavior without adding diagnostic text to the
model request. Use `/status` for the current pane and `/copy-trace-log` after
enabling debug or trace logging when request-by-request detail is needed.

## Cache reuse metrics

`Cumulative cache hit` is the token-weighted ratio across all provider samples
retained for the pane. It includes cold starts and earlier uncached requests, so
it can stay low even after the provider cache becomes warm. The per-model token
tables use the same cumulative semantics and label the column explicitly.

`Latest request cache hit` describes exactly one concrete request from the
execution model. Routing and automatic model-sizing calls contribute to their
own cumulative provider/model accounting rows but do not replace this latest
execution sample. An omitted provider cache counter is shown as `unknown`; an
explicitly reported zero is shown as `0.00%`.

Compaction intentionally creates a new immutable summary epoch and can make the
first request afterward cold. A later warm latest-request value does not erase
that cold request from the cumulative value.

## Immutable-context continuity

The status and provider trace surfaces report sensitive-content-free context
diagnostics:

- approximate immutable and volatile token counts;
- the byte length and SHA-256 digest of the immutable projection;
- the longest exact immutable block prefix shared with the previous request;
- whether immutable chronology is append-only; and
- a transition reason: `new_turn`, `compaction`, `provider_switch`,
  `model_switch`, `append_only`, or `unexpected_rewrite`.

The projection covers stable instructions plus settled chronological evidence.
Ephemeral pane, scheduler, approval, retry, and readiness state is excluded.
Only lengths, roles, lifecycle metadata, and cryptographic digests are retained;
the diagnostics do not copy prompt or transcript text.

`unexpected_rewrite` means previously settled immutable chronology no longer
matches and no compaction or provider/model transition explains the change. It
is a correctness diagnostic, not proof that the provider cache itself accepted
or rejected a prefix.

## Provider trace fields

Provider response traces contain separate `usage` and `latest_request_usage`
objects. The first labels its cache ratio as cumulative; the second reports the
latest request ratio while preserving unknown versus explicit-zero counters.
Request traces include `context_continuity` with the same token estimates,
projection digest, common-prefix measurements, append-only flag, and break
classification shown by `/status`.
