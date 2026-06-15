---
name: add-issues
description: Use when recent findings should be turned into mez issue tracker entries.
---

Review the recent findings in the current task context. Group related findings into the smallest useful set of issues.

Query the mez issue tracker for obvious duplicates before adding new issues.

Use the local issue-tracker MAAP actions directly when they are available in the current action surface: `issue_add` creates an issue, `issue_query` searches existing issues, and `issue_delete` removes an issue record.

If the current action surface does not include issue actions, request the `issues` capability before proceeding. Do not guess at shell or MCP substitutes for local mez issue-tracker work.

Create concise issue titles. In each issue body, capture the observed problem, impacted area, evidence, and the smallest useful next step.

When adding an issue, set `kind` to `defect` for bugs or `task` for planned follow-up work. Provide a single-line `title` and use `body` for the supporting details.

Prefer one issue per distinct bug, gap, or follow-up task. Avoid filing speculative issues without concrete findings.
