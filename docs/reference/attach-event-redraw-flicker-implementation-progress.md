# Attach event redraw flicker implementation progress

This document tracks the implementation of
`docs/reference/attach-event-redraw-flicker-refactor-plan.md`. The work fixes
the flicker introduced by the idle-efficiency refactor's auxiliary event-socket
wakeup path while preserving low idle CPU and event-driven promptness.

## Current approach

- Decode the existing auxiliary event stream as framed JSON-RPC notifications
  instead of treating any readable bytes as a visible redraw.
- Classify each event into an attach render action:
  - `None`: no visible redraw is needed.
  - `View`: request `terminal/view` while preserving the previous output frame
    so normal diff-based rendering remains available.
  - `InvalidateAndView`: invalidate the output frame, then request
    `terminal/view`.
  - `Disconnect`: exit the attach loop cleanly.
- Coalesce complete event frames available in one readable burst and apply only
  the strongest action.

## Progress

- Created this progress document before code changes.
- Added a stateful attach event stream decoder in `src/cli/attach.rs` that
  buffers partial JSON-RPC frames and coalesces complete event bursts into a
  single render action.
- Changed the primary attach loop so ordinary visible event redraws preserve
  the previous output frame and only structural events call
  `invalidate_output_frame()`.
- Added regression coverage for visible events, non-visible diagnostic events,
  structural events, split event frames, and event burst coalescing.

## Validation log

- `cargo fmt --all --check` passed.
- `cargo test --all-targets --all-features attach` passed.
- `just fmt` passed.
- `just check` passed.
- `just clippy` passed.
- `just test` passed.
