# Routed `/loop` Lifecycle

`/loop` is one logical job even when pane-local model routing is enabled. The
runtime classifies the job once, pins the selected worker profile, and returns
one terminal result through the invoking model. Provider-independent
`mez-agent` policy plans handoff, repair, cancellation, and presentation
transitions; the product runtime executes their provider, scheduler, pane, and
persistence effects. Internal iterations are not independent routed turns.

## Lifecycle

| Phase | Owner | Invariant |
| --- | --- | --- |
| Invocation | Invoking pane | Parse `/loop` once, capture the original prompt, mode, parent conversation, and limit, then queue the first work turn. |
| Classification | Auto-sizing router | Classify the first work turn once. The router cannot execute user actions. |
| Iteration | Managed worker | Run the transferred work prompt in one persistent worker with the selected profile pinned. Internal continuation turns skip routing. |
| Exit and handoff | `mez-agent` policy, product runtime, and managed worker | Continue after a completed iteration that emitted `apply_patch`; otherwise stop after the first patch-free completion or at the limit. Preserve the exact final output and request one structured handoff from the same worker. |
| Presentation | Invoking model | Queue the waiting parent for fair capacity reacquisition, then resume it on its ordinary profile and present one result. A joined subagent or macro dependency remains pending through this phase. |

For classification, the product resolves configured profiles and calls the
concrete provider. `mez-agent` builds the bounded router request, validates the
response or fallback, selects the profile, projects actual router usage, and
constructs the managed-worker selection returned to the runtime actor. Provider
credentials, transport errors, telemetry, and actor mutation remain product
effects.

The worker receives a structured transfer of the loop prompt and context. It
does not receive a new literal `/loop` command, so it cannot accidentally
create a nested controller. Routing is disabled for the managed worker's
internal turns, including continuation and handoff requests.

While the parent waits for classification, worker execution, handoff, or a
runtime-owned macro dependency, it retains lifecycle and pane ownership but
releases its active scheduler slot. Each dependent stage can therefore run when
`agents.max_concurrent_agents = 1`. Once the dependency is ready, the parent
returns to the ordinary fair scheduler queue and receives a provider task only
after it reacquires capacity.

## Conversation modes

| Command form | Iteration baseline | Terminal behavior |
| --- | --- | --- |
| `/loop PROMPT` | Reuse the managed worker conversation across iterations. | Present through the invoking parent conversation. |
| `/loop --fork PROMPT` | Create every iteration from the same captured parent transcript high-water mark. Attempts cannot see earlier attempts. | Restore the invoking parent conversation before handoff and keep attempt conversations ephemeral. |
| `/loop --new PROMPT` | Create every iteration with no parent transcript source. | Restore the invoking parent conversation before handoff and keep attempt conversations ephemeral. |

`--limit N` overrides `agents.loop_limit` for that logical job. Reaching the
limit after patch work is a normal terminal condition; it does not classify a
new job or select another worker.

## Failure and cancellation

Worker provider failures, continuation-queue failures, invalid or failed
handoffs, and failed parent presentation all converge on one typed transition
policy. `mez-agent` classifies the next effect without performing I/O; the
runtime stores the stage diagnostic and may queue one response-only explanation
on the invoking model. If that explanation fails or is interrupted, no further
recovery request is allowed.

Handoff, handoff repair, parent presentation, and failure explanation are typed
provider interaction modes. Their static response rules use the selected
mode-specific system instruction. Validated worker results, rejected handoffs,
and stage diagnostics remain immutable chronological evidence or bounded
factual live state; they are not ad-hoc user prompts and do not move the
original parent prompt after its evidence.

Child cancellation resumes the blocked parent once for an explanation. Parent
cancellation interrupts the active worker and releases the loop controller,
conversation indexes, provider tasks, authority, and pane-close ownership.
Late or replayed worker and presentation results are handled as no-ops.

For joined subagent and macro steps, internal worker completion and structured
handoff do not resolve the parent action. The dependency resolves once, after
successful presentation, or fails once after terminal explanation or
cancellation.

Successful parent presentation persists the validated handoff summary once as
a typed transcript context event immediately before the visible parent answer.
Later parent turns and captured forked/routed conversations replay both records;
the exact worker result and presentation-only instructions remain turn-local.

## Examples

Reuse one routed worker until a patch-free verification pass:

```text
/loop implement the parser fix and rerun the focused tests
```

Give every attempt the same parent baseline and cap the job at four attempts:

```text
/loop --fork --limit 4 review and correct the migration
```

Run isolated attempts with no prior transcript:

```text
/loop --new diagnose the minimal reproduction
```

Inside a macro, the entire loop remains one step; the macro judge receives the
terminal presented result rather than an intermediate worker iteration.
