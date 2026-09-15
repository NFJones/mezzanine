---
name: fix-issues
description: Work the current project's mez issues to verified resolution in the main agent, using one persistent subagent only for independent review.
---

Maintain one outer control loop for the entire run. Query `in-progress` issues first, then `open` issues. Reuse query results only until the issue store changes. Do not stop after completing or blocking one issue.

Work one dependency-ready issue at a time. Prioritize, in order:
1. in-progress issues related to uncommitted changes;
2. other in-progress issues;
3. open issues related to uncommitted changes;
4. other open issues.

An issue is workable when it is dependency-ready and has no recorded external blocker that prevents further action in the current run. Maintain a per-run deferred set containing every currently unworkable issue id, its blocker or unresolved dependency, and the exact condition that permits retry. Do not reselect a deferred issue until that condition or relevant repository or issue state changes.

Establish whether dirty files relate to an issue with repository evidence; do not assume every uncommitted change is related. Work prerequisites before dependent issues. Select the highest-priority workable issue not in the deferred set. Mark a selected open issue `in-progress` before implementation.

Every outer-loop iteration must end in one of these observable outcomes:
- the active issue is verified and marked `resolved`;
- the active issue makes measurable progress and continues through its next phase;
- a prerequisite issue is created or advanced and becomes the next eligible work;
- the active issue is added to the deferred set with a concrete retry condition.

Never immediately repeat an iteration with the same issue, phase, and unchanged evidence.

The main agent owns all control, issue selection, repository inspection, planning, implementation, validation, issue state and notes, repair decisions, and final resolution decisions. Do not delegate planning, implementation, coordination, or issue management.

For each issue, inspect enough code, tests, docs, and specifications to make a concrete plan. Record and maintain concise issue notes containing:
- problem and intended fix surface;
- implementation checklist and status;
- validation steps and results.

Provision exactly one reusable review subagent with `spawn_agent`, using `role: explorer` and `lifetime: persistent`. The review subagent must always use a large model: set `size: large` and `reasoning_effort: high`. Give it a durable review-only objective and an initial prompt stating that it must not plan, implement, edit files, manage issues, or coordinate other agents. Discover its identity with `list_agents` and reuse it for every issue.

Keep MMP communication simple:
1. After the main agent has implemented and validated an issue, discover the persistent reviewer and send it one review assignment via MMP `send_message` containing the issue id, intended behavior, relevant diff, and validation evidence. A successful send only means the assignment was queued; local prose, pane output, task-status traffic, and implicit handoffs are not review completion.
2. The reviewer must return its verdict to the requesting main agent via MMP `send_message`, correlated to the assignment when an assignment message id is available; it must not report a review only in its local pane or task result.
3. Require only these reply fields: issue id, verdict (`pass` or `changes-requested`), findings ordered by severity, and any validation gaps.
4. Use `wait` as the only executable action only while that required substantive reviewer MMP reply is outstanding and the pipeline cannot proceed without it. Runtime task-status notifications are not substitutes for reviewer messages. Do not create relays or ask the reviewer to message another agent.

Treat stale or mismatched replies as non-authoritative. The reviewer remains available for later assignments and follow-up questions. If review requests changes, the main agent decides which findings apply, implements and validates the repairs, then sends the updated result back to the same reviewer. Review must pass before resolution.

Implement the complete fix and add or update focused regression coverage when feasible. Every issue plan must include validation steps; run them before review and resolution, and record the outcome.

When an error is discovered, fix it within the active issue plan if it is related to that issue. If it is unrelated, create a new defect issue with the observed error and continue the active issue. Do not abandon the outer loop when new issues are created.

Update issue notes when the plan, progress, validation result, review result, or next action changes. After successful validation and passing review, record both outcomes and mark the issue `resolved`; do not delete it. If blocked, record the blocker and retry condition, add the issue to the deferred set, and immediately continue selection from the remaining query results.

After every resolution, deferral, dependency change, or newly created issue, query both states again and restart selection at the top of the priority order. Reconsider deferred issues only when their recorded retry conditions or relevant state have changed. Continue until every `in-progress` and `open` issue is either resolved or present in the deferred set with a concrete external blocker or unresolved dependency, and both fresh queries contain no other workable issue.

Once fresh queries confirm that no workable issue remains, close the persistent review agent before producing the final report. Do not leave the reviewer running after the outer loop ends.

In the final report, list resolved issues, deferred issues with retry conditions, and validation plus review evidence for each resolution.
