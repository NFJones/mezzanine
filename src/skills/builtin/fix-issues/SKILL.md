---
name: fix-issues
description: Use when you need to query the current project's mez issue tracker, fix open issues, keep per-issue plans and progress notes updated, and mark verified fixes resolved.
---

Query open issues in the mez issue tracker for the current project first. Use local issue actions when they are exposed; otherwise request the issues capability. If the open query returns no issues, stop and take no further action. Inspect returned `state` and `depends_on` metadata, and work dependency-free prerequisite issues before issues that depend on them.

Work one returned issue at a time, choosing an issue whose `depends_on` list is empty or already resolved. Before implementing, inspect the cited code, tests, docs, and spec enough to form a concrete execution plan for that issue. If all remaining issues are blocked by dependencies that are absent from the query result, query or inspect the missing issue ids before proceeding; report a blocker if the dependency graph cannot be resolved.

Store the plan in the issue notes field with a progress-tracker section. Keep the notes concise and structured for multi-turn updates. At minimum include the problem summary, intended fix surface, validation steps, and a checklist or status list that can be revised as work advances.

Use issue_update to refresh the notes whenever the plan changes, a step completes, validation fails, or the next action changes. Keep the issue notes current instead of creating separate scratch tracking when the issue record can hold the progress state.

Implement the fix completely. Add or update focused regression coverage first when feasible, then broaden validation proportionally.

After the fix is verified, update the issue notes with the completed validation outcome, then mark the issue `resolved` with `issue_update` so history remains queryable. Do not delete an issue merely because it has been fixed.

Repeat until the project open-issue query returns no remaining open issues. This skill must be loop-friendly: when there are no open issues left, take no action. Do not mark an issue resolved until its own implementation and verification are complete.
