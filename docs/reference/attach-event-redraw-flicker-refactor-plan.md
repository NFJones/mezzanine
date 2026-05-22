# Attach event redraw flicker refactor plan

This document records the corrective refactor plan for the rapid UI flicker
introduced while making the control-socket primary attach path event-driven.
The work supersedes the optional caching and benchmarking tail of the idle
efficiency refactor until the default attached-client rendering path is stable.

## Problem

The default `mez attach` path can connect to the auxiliary runtime event socket.
The current event-driven attach loop treats any readable event-stream chunk as a
visible redraw request. That is too broad: the event stream can include noisy
runtime bookkeeping, metrics, persistence, lifecycle, foreground process,
message, hook, and timer notifications that do not necessarily change the
visible terminal frame.

The symptom is worse because the attach loop invalidates the terminal output
frame cache before rendering every event-driven view. That prevents the terminal
writer from using the previous frame as a diff base, so repeated generic events
can become repeated full-frame writes and visible flicker.

## Objective

Fix the flicker without returning to fixed periodic idle repainting.

- Keep idle CPU low for attached clients.
- Keep pane output prompt.
- Avoid terminal redraws for generic event-socket traffic.
- Preserve incremental terminal diffing for ordinary pane output.
- Reserve output-frame cache invalidation for structural redraws only.
- Keep default settings safe; no user configuration flag should be required to
  avoid flicker.

## Design

Runtime event input should be classified before it can trigger rendering.
The attach loop should model event rendering with an explicit action enum:

- `None`: the event is not visibly relevant to the attached terminal.
- `View`: visible content or status may have changed; request `terminal/view`
  while preserving the previous output frame for diff rendering.
- `InvalidateAndView`: the rendering basis changed structurally; invalidate the
  output frame, then request `terminal/view`.
- `Disconnect`: the event stream closed or the runtime is unavailable; exit the
  attach loop cleanly.

The action ordering for coalesced event bursts is:

```text
Disconnect > InvalidateAndView > View > None
```

## Phase 1: add an event-to-render classifier

Add a local classifier around runtime event-socket reads in `src/cli/attach.rs`.
If the existing event payload is structured enough, decode it and classify each
event. If the payload is not structured enough, add a minimal runtime-side
event envelope or visible-render generation instead of preserving the current
raw "any bytes means repaint" shortcut.

Conservative initial classifications:

- `InvalidateAndView` for resize, layout tree changes, active window/pane
  structure changes, theme/config/frame changes, attach/session reset, and
  mode changes that alter framing, alternate screen behavior, mouse regions, or
  status layout.
- `View` for visible pane output, visible active-pane metadata changes, visible
  agent status/footer changes, and configured clock/uptime/status ticks when
  the displayed resolution has advanced.
- `None` for metrics, persistence/checkpoint notifications, hook lifecycle
  updates that do not alter visible UI, input acknowledgements, background-pane
  events not visible to the attached client, message/control bookkeeping, and
  timer wakes that do not affect a visible configured field.

## Phase 2: stop unconditional output-frame invalidation

Change the attach loop from the current behavior:

```text
runtime event -> invalidate_output_frame() -> terminal/view
```

to:

```text
None              -> no render
View              -> terminal/view only
InvalidateAndView -> invalidate_output_frame(); terminal/view
Disconnect        -> clean attach-loop exit
```

This is the direct flicker fix. Ordinary pane output should render without
clearing the previous frame cache.

## Phase 3: coalesce runtime event bursts

When the event socket becomes readable, drain immediately available event data
and compute the strongest render action once. The loop should issue at most one
`terminal/view` per event burst. This avoids multiple consecutive frames for a
single logical update burst such as pane output plus metadata plus persistence
bookkeeping.

## Phase 4: add visible render generations if classification is insufficient

If event classification still allows duplicate redraws, add runtime-maintained
generation counters:

- `visible_render_generation`: increments only when visible client state
  changes.
- `structural_render_generation`: increments only when the previous output
  frame is no longer a safe diff basis.

The attach client can then remember the last rendered generations and skip
duplicate `terminal/view` requests unless startup, resize, or a generation
advance requires a redraw.

## Regression tests

Add focused tests around the attach loop and classifier:

1. A generic runtime event does not issue `terminal/view` and does not call
   `invalidate_output_frame()`.
2. A visible pane-output event issues one `terminal/view` without invalidating
   the output frame.
3. A structural event invalidates once and issues one `terminal/view`.
4. A burst of mixed events is coalesced into at most one render, with the
   strongest render action winning.
5. Default idle attach with no input and no visible event stays quiet.
6. Event-stream disconnect while idle exits cleanly without broken-pipe noise.

## Manual verification

Verify the fixed default path manually:

- Idle attached shell for 30-60 seconds: no flicker, no steady render counter
  climb, low CPU.
- Steady command output: output appears promptly without full-screen flashing.
- Interactive typing: no noticeable input latency regression.
- Terminal resize: immediate clean redraw.
- Pane/window switching, if available: one structural redraw per change.
- Detach and reattach: first frame is correct.
- Agent mode inactive: no animation-driven redraws.
- Agent UI active: visible status updates still render.

## Metrics and closeout

Use the runtime metrics added during the idle efficiency refactor to confirm:

- idle `terminal/view` requests per second are approximately zero;
- idle full invalidations per second are zero;
- visible pane output increments view counts but not invalidation counts;
- resize/layout changes increment structural invalidation counts;
- CPU remains near the post-idle-refactor target and does not regress to fixed
  100 ms polling behavior.

Update `docs/reference/mez-idle-efficiency-implementation-progress.md` when the
fix is implemented, including validation results and any remaining follow-up
items.

## Suggested commit sequence

1. `Classify attach event redraw actions.`
2. `Avoid full invalidation for attach event redraws.`
3. `Coalesce attach event redraw bursts.`
4. Optional: `Track visible render generations.`
5. `Document attach flicker fix verification.`
