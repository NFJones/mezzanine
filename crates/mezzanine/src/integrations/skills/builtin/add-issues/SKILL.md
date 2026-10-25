---
name: add-issues
description: Use when recent findings should be turned into mez issue tracker entries.
---

Review all recent findings and relevant evidence available in the current task context before drafting issues. Group related findings into the smallest useful set of issues.

Query the mez issue tracker for obvious duplicates before adding new issues.

Use the local issue-tracker MAAP actions directly when they are available in the current action surface: `issue_add` creates an issue, `issue_query` searches existing issues, `issue_update` refreshes issue metadata such as dependencies, and `issue_delete` removes an issue record.

If the current action surface does not include issue actions, request the `issues` capability before proceeding. Do not guess at shell or MCP substitutes for local mez issue-tracker work.

Create concise issue titles. In each issue body, capture enough context for a future agent to complete the work without rereading the original transcript: the observed symptom or desired follow-up, impacted area, current-turn evidence or source, reproduction steps or trigger when known, a full implementation plan, relevant constraints and validation expectations, and any context needed to avoid redundant rediscovery. Do not create an issue from a vague memory of a finding; if the current context does not contain concrete evidence, first inspect or ask for the missing evidence instead of filing a low-context issue.

## Completion readiness

Every filed issue must have a clear objective: state the desired outcome, bounded scope and observable acceptance criteria that distinguish completion from partial progress. The implementation plan, prerequisites and validation must make the issue workable to completion by an agent, not merely describe an investigation or require an undefined future signoff. For investigations, define the expected RCA, justified fixes or mitigation, and an explicit disposition for remaining uncertainty.

Before filing, check whether an agent can obtain the needed evidence, make the required decisions, implement the work and verify the acceptance criteria using available tools, permissions, source and public documentation. Do not invent access, assume user decisions, weaken safety constraints or require unavailable live/vendor/platform certification when feasible local verification can satisfy the objective. Distinguish missing implementation from checks that cannot run, and specify how verification limits will be reported.

If these requirements cannot yet be satisfied, you may still file an issue to preserve a concrete finding or desired follow-up. Mark it clearly as **Needs user clarification**, state the best-known objective and provisional plan, and list the unresolved decisions, information, access or external dependencies that prevent agent completion. Explain what must be resolved for each blocker and how it affects scope or acceptance. Do not describe the issue as ready or imply that a provisional objective or plan is final.

In that case, ask the user specific questions in the user-facing response and identify the affected issue ids; recording questions only in the issue body is not sufficient. First discover facts safely available from the current context or tools rather than asking the user to rediscover them. After the user answers, update the filed issue with the clarified objective, executable plan, real prerequisites and acceptance criteria; retain any still-unresolved blockers explicitly. Do not silently substitute a different objective or close the issue merely because the questions were asked.

When adding an issue, set `kind` to `defect` for bugs or `task` for planned follow-up work. Provide a single-line `title`, use `body` for the supporting details, and set `depends_on` to the issue ids that are hard prerequisites. Use an empty dependency list when there are no prerequisites.

Create prerequisite issues before dependent issues. When a new issue depends on another new issue, add the prerequisite issue first with an empty or already-known `depends_on` list, wait for the returned real issue id, and only then add the dependent issue in a later action batch using that returned id in `depends_on`. Do not invent, predict, or placeholder dependency ids in the same batch.

Prefer one issue per distinct bug, gap, or follow-up task. Distinguish hard prerequisites from merely related follow-up work; only hard prerequisites belong in `depends_on`. Avoid creating dependency cycles. If `issue_add` rejects a cyclic or nonexistent dependency, inspect the returned dependency id and error first, query or validate the referenced issue when needed, correct the dependency list, and retry `issue_add`. Use the same title and body on a corrected retry, but first query for a matching issue if the prior result was ambiguous so the retry cannot create a duplicate. Report a blocker only when the dependency cannot be safely corrected from current evidence. Avoid filing speculative issues without concrete findings.
