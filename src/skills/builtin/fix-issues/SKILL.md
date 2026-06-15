---
name: fix-issues
description: Use when you need to query the current project's mez issue tracker, fix the returned issues, keep per-issue plans and progress notes updated, and remove verified fixed issues from the tracker.
---

Query the mez issue tracker for the current project first. Use local issue actions when they are exposed; otherwise request the issues capability. If the query returns no issues, stop and take no further action.

Work one returned issue at a time. Before implementing, inspect the cited code, tests, docs, and spec enough to form a concrete execution plan for that issue.

Store the plan in the issue notes field with a progress-tracker section. Keep the notes concise and structured for multi-turn updates. At minimum include the problem summary, intended fix surface, validation steps, and a checklist or status list that can be revised as work advances.

Use issue_update to refresh the notes whenever the plan changes, a step completes, validation fails, or the next action changes. Keep the issue notes current instead of creating separate scratch tracking when the issue record can hold the progress state.

Implement the fix completely. Add or update focused regression coverage first when feasible, then broaden validation proportionally.

After the fix is verified, update the issue notes with the completed validation outcome, then delete the issue from the mez issue tracker. Do not delete an issue until implementation and verification are complete.

Repeat until the project issue query returns no remaining issues. This skill must be loop-friendly: when there are no issues left, take no action.
