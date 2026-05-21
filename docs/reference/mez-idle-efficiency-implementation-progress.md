# Mez idle efficiency implementation progress

This document tracks the multi-phase refactor to reduce idle CPU in attached
Mezzanine clients, especially when agent mode is inactive.

## Problem

The control-socket `mez attach` primary client path currently polls terminal
input with a fixed timeout and sends `terminal/step` with `render: true` even
when no terminal input, resize, or runtime render event has occurred. That can
cause roughly ten full control/render round trips per idle attached client per
second. Those renders recompute terminal frame context, status fields, mouse hit
regions, pane frames, and other view data even when nothing visible changed.

## Implementation sequence

1. Add focused regression coverage and lightweight evidence for the idle attach
   behavior.
2. Stop no-input timeout iterations from sending `terminal/step render=true` in
   the control-socket primary attach loop.
3. Add or reuse a runtime render-invalidation signal so pane output, lifecycle
   changes, config/frame changes, resize, and agent/status changes still wake
   attached clients promptly.
4. Split input handling from rendering semantics so input-only steps and
   render/view requests are explicit.
5. Throttle time-dependent UI invalidations such as clock, uptime, cursor blink,
   and agent timers to visible resolutions, and avoid animation-driven idle work
   when agent mode is inactive or reduced motion is enabled.
6. Cache stable render inputs only after the event-driven attach path is in
   place, with invalidation tied to layout, frame, mouse, config, active pane,
   and relevant agent-status changes.

## Validation checklist

- `cargo check --all-targets --all-features` while iterating.
- Focused tests for control-socket attach/render behavior.
- Final `just fmt`, `just clippy`, and `just test`.
- Manual validation that idle attached sessions do not emit repeated render
  requests, pane output appears promptly, typing latency remains acceptable,
  resize redraws immediately, agent/status UI still updates, and detach/reattach
  still works.

## Current status

- Created this progress document before implementation, per repository guidance
  for large multi-phase refactors.
- Implemented the first control-socket primary attach-loop slice: initial or
  resize-driven idle iterations request an explicit `terminal/view`, while later
  terminal-input timeouts do not send repeated render requests.
