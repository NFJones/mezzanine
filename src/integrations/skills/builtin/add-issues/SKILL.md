---
name: add-issues
description: Use when recent findings should be turned into mez issue tracker entries.
---

Review all recent findings and relevant evidence available in the current task context before drafting issues. Group related findings into the smallest useful set of issues.

Query the mez issue tracker for obvious duplicates before adding new issues.

Use the local issue-tracker MAAP actions directly when they are available in the current action surface: `issue_add` creates an issue, `issue_query` searches existing issues, `issue_update` refreshes issue metadata such as dependencies, and `issue_delete` removes an issue record.

If the current action surface does not include issue actions, request the `issues` capability before proceeding. Do not guess at shell or MCP substitutes for local mez issue-tracker work.

Create concise issue titles. In each issue body, capture enough context for a future agent to complete the work without rereading the original transcript: the observed symptom or desired follow-up, impacted area, current-turn evidence or source, reproduction steps or trigger when known, a full implementation plan, relevant constraints and validation expectations, and any context needed to avoid redundant rediscovery. Do not create an issue from a vague memory of a finding; if the current context does not contain concrete evidence, first inspect or ask for the missing evidence instead of filing a low-context issue.

When adding an issue, set `kind` to `defect` for bugs or `task` for planned follow-up work. Provide a single-line `title`, use `body` for the supporting details, and set `depends_on` to the issue ids that are hard prerequisites. Use an empty dependency list when there are no prerequisites.

Create prerequisite issues before dependent issues. When a new issue depends on another new issue, add the prerequisite issue first with an empty or already-known `depends_on` list, wait for the returned real issue id, and only then add the dependent issue in a later action batch using that returned id in `depends_on`. Do not invent, predict, or placeholder dependency ids in the same batch.

Prefer one issue per distinct bug, gap, or follow-up task. Distinguish hard prerequisites from merely related follow-up work; only hard prerequisites belong in `depends_on`. Avoid creating dependency cycles, and report the blocker if the tracker rejects a cyclic or nonexistent dependency. Avoid filing speculative issues without concrete findings.
