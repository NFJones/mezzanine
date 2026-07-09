---
name: add-issues
description: Use when recent findings should be turned into mez issue tracker entries.
---

Review the recent findings in the current task context. Group related findings into the smallest useful set of issues.

Query the mez issue tracker for obvious duplicates before adding new issues.

Use the local issue-tracker MAAP actions directly when they are available in the current action surface: `issue_add` creates an issue, `issue_query` searches existing issues, `issue_update` refreshes issue metadata such as dependencies, and `issue_delete` removes an issue record.

If the current action surface does not include issue actions, request the `issues` capability before proceeding. Do not guess at shell or MCP substitutes for local mez issue-tracker work.

Create concise issue titles. In each issue body, capture the observed problem, impacted area, evidence, and the smallest useful next step.

When adding an issue, set `kind` to `defect` for bugs or `task` for planned follow-up work. Provide a single-line `title`, use `body` for the supporting details, and set `depends_on` to the issue ids that are hard prerequisites. Use an empty dependency list when there are no prerequisites.

Create prerequisite issues before dependent issues. When a new issue depends on another new issue, add the prerequisite issue first with an empty or already-known `depends_on` list, wait for the returned real issue id, and only then add the dependent issue in a later action batch using that returned id in `depends_on`. Do not invent, predict, or placeholder dependency ids in the same batch.

Prefer one issue per distinct bug, gap, or follow-up task. Distinguish hard prerequisites from merely related follow-up work; only hard prerequisites belong in `depends_on`. Avoid creating dependency cycles, and report the blocker if the tracker rejects a cyclic or nonexistent dependency. Avoid filing speculative issues without concrete findings.
