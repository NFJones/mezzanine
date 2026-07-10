# Agent Execution Flow

This document describes how one Mezzanine agent prompt moves from pane-local
input to provider request, MAAP action execution, transcript feedback, and turn
completion. It is intended for contributors who need to understand the agent
harness boundaries before changing prompts, action handling, permissions,
runtime scheduling, or provider adapters.

The normative user-facing behavior remains in [SPEC.md](../SPEC.md). This file
focuses on the implementation flow and the invariants that keep the model,
runtime, pane shell, and durable stores separated.

## Main implementation boundaries

The agent implementation is split across a small number of ownership layers:

- `src/agent/mod.rs` is the public facade for agent primitives. It re-exports
  session state, turn records, model request assembly, provider adapters, MAAP
  types, action planning/execution helpers, readiness decisions, shell
  bootstrap helpers, and slash command handling.
- `src/agent/session.rs` owns pane-local agent shell session state: visibility,
  running turn id, transcript counts, log level, directive, permission display,
  and related command output helpers.
- `src/agent/turn.rs` owns `AgentTurnRecord` and `AgentTurnLedger`. The ledger
  enforces valid state transitions such as queued, running, blocked, completed,
  failed, and interrupted.
- `src/agent/slash.rs` parses and executes agent-shell slash commands. Commands
  that require live runtime resources return `RequiresRuntime` so runtime code,
  rather than the static parser, performs the side effect.
- `src/agent/context/assembly.rs` converts the runtime-collected
  `AgentContext` into a provider-facing `ModelRequest`, including the system
  prompt, repository instructions, context block roles, prompt-cache metadata,
  MCP summary metadata, and the initial allowed action surface.
- `src/agent/actions/runner.rs` owns model turn negotiation. It sends the model
  request, validates and repairs MAAP responses, handles capability negotiation,
  applies default action gates, produces initial `ActionResult` values, and
  determines whether the turn is still running or terminal.
- `src/agent/actions/execution.rs` owns transport-neutral action execution
  helpers. It converts accepted local or MCP action plans into concrete executor
  calls and returns durable `ActionResult` records.
- `src/agent/semantic/mod.rs` and sibling semantic helpers classify local MAAP
  actions such as `shell_command` and `apply_patch` into executable plans.
- `src/async_runtime/client.rs` owns async provider-worker dispatch. It calls
  `AgentTurnRunner::run_turn_async_ref_with_allowed_actions`, dispatches
  runtime-executed local actions, and turns worker results into runtime events.
- `src/runtime/agent/*` owns the live runtime lifecycle around agent turns:
  starting and finishing turns, applying runtime-only side effects, appending
  action-result feedback to context, subagent handling, outcome shaping, hooks,
  and scheduler drain behavior.
- `src/control/*` exposes agent state and control-plane commands to clients;
  it should not duplicate agent turn business rules that belong to the agent or
  runtime layers.

## High-level sequence

One normal agent turn follows this sequence:

```text
pane-local prompt or queued task
  -> agent shell command/session handling
  -> runtime creates or resumes an AgentTurnRecord
  -> runtime assembles AgentContext blocks
  -> AgentTurnRunner builds a ModelRequest
  -> provider adapter returns a ModelResponse with a MAAP batch
  -> runner validates, repairs, or negotiates capabilities
  -> runner plans model actions into ActionResult records
  -> runtime executes running local/MCP/subagent/config/network actions
  -> runtime appends action results to the turn context
  -> another provider turn runs, or the ledger records a terminal state
```

The model never executes local side effects directly. It emits a MAAP batch;
Mezzanine validates the batch against the current action surface and runtime
state, then the owning runtime subsystem performs any accepted side effects.

## Prompt and slash-command entry points

Agent-shell input is pane-scoped. Slash commands are parsed by
`parse_slash_command` and executed through
`execute_agent_shell_command_with_context` in `src/agent/slash.rs`.

Important command-flow rules:

- Non-slash input is not consumed by the slash command parser and can become a
  normal model turn.
- Slash commands that mutate only session-owned state, such as directive or log
  level changes, can return a local `Mutated` or `Display` outcome.
- Slash commands that need live runtime state return `RequiresRuntime`. Examples
  include session listing, macro discovery, modified-file listing, context
  export, trace-log export, and patch export.
- Commands that are not queueable while a turn is active fail fast if the
  session already has a running turn id.

This split keeps command parsing deterministic while preserving runtime
ownership of filesystem, pane, provider, and persistence side effects.

## Turn records and scheduling state

`AgentTurnRecord` describes one scheduled or running unit of agent work. It
includes the turn id, agent id, pane id, trigger, policy profile, model profile,
optional parent turn id, cooperation mode, state, and optional initial
capability.

`AgentTurnLedger` owns state transitions:

- `queue_turn` records a queued turn.
- `mark_turn_running` moves an existing queued turn to running.
- `start_turn` inserts a new running turn.
- `finish_turn` records a terminal or blocked state.
- `resume_blocked_turn` moves a blocked turn back to running.

When concurrent turns are disabled, the ledger rejects attempts to run a second
turn for the same agent. Terminal records are retained only up to the ledger's
retention cap so long-running sessions do not grow without bound.

## Context assembly and provider request construction

Runtime code collects context blocks before asking the agent harness to build a
provider request. `assemble_model_request` in `src/agent/context/assembly.rs`
performs the final projection from `AgentContext` to `ModelRequest`.

The assembly step:

- validates that the selected model provider, model, and turn id are present;
- prepares context blocks for provider consumption;
- embeds repository guidance into the system prompt, with provider-specific
  handling for DeepSeek;
- preserves provider-native transcript replay events as system messages;
- omits configuration identity blocks that are used for prompt-cache metadata
  rather than model-visible conversation content;
- converts each remaining context block into a provider message with the role
  selected by `role_for_source`;
- recovers MCP prompt summary counts from the runtime's MCP integrations block;
- attaches prompt-cache session and lineage identifiers; and
- starts with `ModelInteractionKind::CapabilityDecision` and
  `AllowedActionSet::capability_decision()` unless the runtime pre-seeds a more
  specific action surface.

Skill-loaded context can further constrain the allowed action set before the
request is sent.

## Provider turn negotiation

`AgentTurnRunner` in `src/agent/actions/runner.rs` owns the provider-facing
loop for one model response. The async runtime calls
`run_turn_async_ref_with_allowed_actions`; tests and synchronous harnesses use
the corresponding synchronous methods.

The runner sequence is:

1. Start the turn in the ledger.
2. Build a `ModelRequest` from the selected model profile, turn record, and
   current context.
3. If the runtime supplied an initial allowed action set, switch the request to
   `ModelInteractionKind::ActionExecution` and use that action surface.
4. Apply default action gates for MCP tools, memory actions, and issue actions.
5. Send the request through the selected provider adapter.
6. Validate that the provider identity in the response matches the selected
   provider.
7. Require a parsed MAAP action batch.
8. Validate the batch against the current allowed actions, batch schema, turn
   metadata, and available MCP tools.
9. If the model requested a capability instead of executable actions, build a
   continuation request with the requested action surface and ask the provider
   again.
10. If the provider output or MAAP batch is repairable, send a bounded repair
    request. Repair prompts are not allowed to become the durable request stored
    in turn execution history.
11. Convert accepted actions into initial `ActionResult` records.
12. Compute the turn state from action results and the batch's final-turn flag.

The runner tracks cumulative token usage and latest quota usage across repair
or continuation attempts so the final execution record reflects all provider
work performed for that turn step.

## MAAP response handling and action surfaces

Providers return model text plus an optional parsed `MaapBatch`. A valid batch
contains a rationale and one or more actions, and it must match the action
surface exposed in the request.

The action surface is intentionally narrow at each step:

- Capability-decision turns expose capability-request actions so the model can
  ask for the correct action family without being given every side effect.
- Action-execution turns expose only the current allowed action set.
- MCP, memory, and issue actions are added only when the corresponding runtime
  gate says they are available.
- Loaded skills can further restrict the visible actions.

The runner rejects mixed or disallowed action batches. Some disallowed-action
cases are converted into capability-continuation requests when the model clearly
asked for a missing action family. Otherwise the runner uses the bounded MAAP
repair path or returns a failed execution summary.

Memory actions have an additional guardrail in the runner: one turn may perform
only a small number of memory searches, and memory actions framed as wrapper
compliance placeholders are skipped with a successful structured result. This
keeps durable memory from becoming a fallback for malformed action-wrapper
behavior.

## Local action planning and execution

The runner itself plans actions but does not own every side effect. Local action
planning classifies model actions into runtime-executable plans, while concrete
execution is delegated to runtime-provided executors.

For local actions:

- `local_action_plan` determines whether an accepted MAAP action is a local
  shell-backed action.
- `execute_local_action` converts the plan and a runtime marker token into an
  executor request.
- `PaneShellLocalExecutor` adapts local plans to pane-shell execution.
- `execute_shell_action_through_pane` and shell transport helpers wrap commands
  so Mezzanine can delimit model-authored execution and decode results.
- `apply_patch` remains a structured MAAP action, not a shell command; semantic
  patch helpers plan and validate patch transactions before execution.

Runtime action execution returns `ActionResult` values. A running result means
the model emitted an accepted action and runtime work remains. A succeeded or
failed result becomes model-visible evidence in the next context assembly pass.

## Runtime worker and feedback loop

The async runtime is the bridge between the harness and live side effects.
`src/async_runtime/client.rs` owns the provider worker path that constructs an
`AgentTurnRunner`, calls `run_turn_async_ref_with_allowed_actions`, and then
executes runtime-owned work for any still-running action results.

Runtime-owned execution includes local shell actions, MCP calls, spawned
subagents, configuration changes, network actions, issue actions, memory
actions, and other effects whose resources live outside the pure provider
adapter. The runtime updates the `AgentTurnExecution` with the observed
`ActionResult` values and computes the next turn state from those results and
the model's final-turn flag.

`src/runtime/agent/outcome.rs` and sibling runtime agent modules then decide
how to continue:

- successful and failed action results are appended to the active turn context
  as `ContextSourceKind::ActionResult` blocks;
- recoverable failures can be fed back to the model for another provider turn;
- blocked work remains blocked until the required approval, child turn, or
  external state becomes available;
- terminal final responses complete the ledger record and update runtime
  counters; and
- subagent observations can update parent turn action results before the parent
  continues.

The feedback loop continues until the model emits a final response with no
running actions, an unrecoverable failure occurs, the turn is interrupted, or
the runtime records a blocked state.

## Readiness, shell, and permission boundaries

Agent execution intentionally separates model output from local authority:

- Pane readiness is tracked separately from provider readiness. Shell-backed
  actions can be delayed or rejected until Mezzanine has confirmed a safe pane
  shell boundary.
- Agent-authored shell commands are validated before they reach the pane shell;
  heredoc redirection and attempts to invoke semantic MAAP actions such as
  `apply_patch` as shell programs are rejected.
- Permission policy and approval stores are passed into `AgentTurnRunner` and
  runtime execution so action planning and action dispatch both see the active
  policy state.
- Path scopes and subagent scope declarations constrain what local actions and
  child agents may affect.
- MCP tools are exposed only when the prompt-local MCP integration context and
  runtime tool gates make them available.

These boundaries are what let Mezzanine show the model useful action surfaces
without letting provider text bypass local permission, shell, MCP, filesystem,
or scheduler controls.

## Transcript and persistence boundaries

The agent transcript and provider transcript are related but not identical.
Normal transcript entries are provider-neutral and user-visible. Provider-native
continuity payloads use hidden transcript events so compatible providers can
replay required state without rendering those payloads as ordinary conversation
text.

Turn execution persistence records the durable request, model response,
provider usage, action results, and terminal state. Repair requests are kept out
of the durable request when a corrected response succeeds, which prevents repair
diagnostics from polluting future context. Runtime-export commands can inspect
the assembled model request or patch/action history through live runtime state,
but those exports are runtime-owned side effects rather than slash-parser work.

## Notable risks when changing the flow

- Do not add provider-specific assumptions to `AgentContext`; keep final
  provider projection in context assembly or provider adapters.
- Do not execute side effects from provider adapters. They should return parsed
  model responses, not mutate panes or runtime stores.
- Do not widen the default action surface just to avoid one continuation round
  trip. Narrow action surfaces are part of the permission and safety model.
- Keep MAAP repair prompts bounded and non-durable unless the repair response is
  itself the durable, valid model response.
- Keep runtime-only resources, such as pane shell handles, MCP transports,
  subagent scheduling, config mutation, memory stores, and issue stores, behind
  runtime executors.
- When adding a new action family, update the MAAP type, action-surface gating,
  semantic planning, runtime execution, action-result formatting, tests, and
  user-facing action reference together.
- When changing turn states, update both `AgentTurnLedger` invariants and the
  runtime state derivation helpers so queued, running, blocked, terminal, and
  interrupted turns remain consistent.
