---
name: fix-issues
description: Use when you need to query the current project's mez issue tracker, fix open issues, keep per-issue plans and progress notes updated, and mark verified fixes resolved.
---

Query open issues in the mez issue tracker for the current project first only when current action-result context does not already contain a successful open-issue query for the current issue-store mutation state. Use local issue actions when they are exposed; otherwise request the issues capability. Treat the latest successful query result as current evidence across provider continuations: do not repeat the query merely because another capability, inspection, edit, test, or provider call occurred. Use `refresh: true` only after concrete evidence that the issue store changed externally. If the open query returns no issues, stop and take no further action. Inspect returned `state` and `depends_on` metadata, and work dependency-free prerequisite issues before issues that depend on them.

Work one returned issue at a time, choosing an issue whose `depends_on` list is empty or already resolved. In the first action batch after choosing, name the selected issue id in the batch rationale and record `Active issue: <id>` plus the durable implementation direction in `thought`. Keep using that selected id until the issue is resolved or explicitly blocked. Before implementing, inspect the cited code, tests, docs, and spec enough to form a concrete execution plan for that issue. Do not issue another open-backlog query while the selected issue is being inspected, implemented, documented, or validated. If all remaining issues are blocked by dependencies that are absent from the query result, use a narrowly filtered query or inspect the missing issue ids before proceeding; report a blocker if the dependency graph cannot be resolved.

Store the plan in the issue notes field with a progress-tracker section. Keep the notes concise and structured for multi-turn updates. At minimum include the problem summary, intended fix surface, validation steps, and a checklist or status list that can be revised as work advances.

Use issue_update to refresh the notes whenever the plan changes, a step completes, validation fails, or the next action changes. Keep the issue notes current instead of creating separate scratch tracking when the issue record can hold the progress state.

Implement the fix completely. Add or update focused regression coverage first when feasible, then broaden validation proportionally.

After the fix is verified, update the issue notes with the completed validation outcome, then mark the issue `resolved` with `issue_update` so history remains queryable. Do not delete an issue merely because it has been fixed.

After resolving or blocking the active issue, query the open backlog again because the successful issue mutation invalidated the earlier snapshot, then select the next dependency-free issue. Repeat until the project open-issue query returns no remaining open issues. This skill must be loop-friendly: within one `/loop` iteration, reuse current query evidence and do not restart discovery after each provider continuation; a later iteration may begin with a fresh query. When there are no open issues left, take no action. Do not mark an issue resolved until its own implementation and verification are complete.
