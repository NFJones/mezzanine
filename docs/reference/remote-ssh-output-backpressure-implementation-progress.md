# Remote SSH Output Backpressure Implementation Progress

## Goal

Improve foreground terminal responsiveness over slow SSH links by ensuring
client output flushing cannot monopolize the attached-terminal loop. User input,
detach commands, and pane I/O must remain responsive while large or frequent
redraws are pending.

## Current Findings

- The production foreground path uses the async attached-terminal service, not
  the synchronous test-only loop.
- Render and flush side effects are already coalesced at actor enqueue time for
  queued effects.
- The remaining stall risk is after a flush effect is drained: the current
  output writer waits until the entire encoded frame has been written before it
  returns.
- Differential frame state is currently committed by the terminal I/O endpoint
  before the frame bytes are fully written. A bounded writer must delay that
  commit until the frame is fully flushed.

## Implementation Plan

1. Add regression coverage for a slow attached output client and prove input is
   still routed while a large frame is only partially writable.
2. Add bounded output write support to the async attached-terminal I/O boundary.
3. Store partially written encoded frames per attached terminal endpoint and
   finish them across later flush passes.
4. Keep queued-frame coalescing for frames that have not started writing; do not
   replace a partially written frame mid-stream.
5. Extend terminal-loop reports with partial-write and pending-byte counters.
6. Add row-span diff optimization after bounded flushing is stable.
7. Add resize-storm proof once the output path changes are validated.

## Progress Log

- Created this progress document before code changes.
- Added bounded attached-terminal output write reporting and a 16 KiB default
  per-flush byte limit at the async terminal I/O boundary.
- Changed the production async attached-terminal endpoint to retain partially
  written encoded frames and commit differential frame state only after the
  retained bytes finish flushing.
- Changed client output flushing to finish pending bytes before draining newer
  flush effects, preserving byte-stream ordering for slow clients.
- Changed the attached-terminal service to wake on retained pending output while
  still allowing input readiness to route pane input between partial writes.
- Extended attached-terminal loop and flush reports with partial-write and
  pending-byte counters.
- Added slow-output regression coverage for both pending-frame ordering and
  input routing while output remains pending.
- Added conservative printable-ASCII changed-span row updates, keeping
  non-ASCII, style-changing, and shrinking rows on the full-row rewrite path.
- Added resize-storm regression coverage that proves debounce timer work is
  rescheduled to the newest foreground size generation.
- Targeted validation run so far:
  - `cargo check --all-targets --all-features`
  - `cargo test --all-targets --all-features async_client_output_flush_service_finishes_pending_output_before_new_frames`
  - `cargo test --all-targets --all-features async_attached_terminal_service_routes_input_while_output_is_pending`
  - `cargo test --all-targets --all-features async_attached_terminal_service_coalesces_resize_storm_timers`
  - `cargo test --all-targets --all-features attached_terminal_output_update_uses_changed_ascii_span_when_safe`
  - `cargo test --all-targets --all-features attached_terminal_output_update_rewrites_wide_glyph_rows`
- Full repository validation completed:
  - `just fmt`
  - `just check`
  - `just clippy`
  - `just test`
  - `git diff --check`

## Checkpoint Status

- The implementation hunks for this task have been staged. The worktree still
  contains unrelated pre-existing edits outside this task, including separate
  edits in `src/async_runtime/tests.rs` and `src/terminal/tests.rs`.
- The scoped change set is ready for commit after the completed validation
  sequence above.
