---
name: fix-issues-parallel
description: Resolve the current project's mez issues in parallel using a main coordinator, medium implementation agents in isolated worktrees, and a large review agent per implementer, with verified integration into the main branch before reassignment.
---

## Coordinator

Act as the coordinator, not the primary implementer. Own issue selection, exclusive assignments, dependency tracking, integration scheduling, issue state, and final resolution. Delegate implementation and repairs to workers.

Inspect repository instructions, status, worktrees, branch topology, and validation requirements. Identify the actual main integration branch from repository evidence; do not assume it is named `main`, change the default branch, or push without authorization. Preserve unrelated user changes. Never discard, overwrite, or include them in issue commits. Establish which dirty changes belong to an issue before assigning that issue; defer assignments whose changes cannot safely be isolated.

Maintain one outer loop until fresh queries show no workable `in-progress` or `open` issues. Query `in-progress` first, then `open`; refresh both after assignments, resolutions, deferrals, new issues, and dependency changes. Select dependency-ready issues in this order:
1. in-progress issues related to uncommitted changes;
2. other in-progress issues;
3. open issues related to uncommitted changes;
4. other open issues.

Maintain an assignment ledger containing issue id, worker and reviewer ids, worktree, branch, base commit, phase, reviewed commit, and integration status. Assign each issue to exactly one worker. Do not assign dependent issues until their prerequisites are integrated and resolved. Serialize issues with overlapping ownership or coupled contracts unless their independence is established.

Keep a per-run deferred set with issue id, concrete blocker or unresolved dependency, and exact retry condition. Reconsider only when that condition or relevant evidence changes. Every loop iteration must dispatch eligible work, advance an active phase, integrate a reviewed change, advance a prerequisite, or defer an unworkable issue. Never repeat an unchanged issue and phase without new evidence.

## Worker pool and worktrees

Spawn multiple reusable implementation agents with `role: worker`, `lifetime: persistent`, `size: medium`, and `reasoning_effort: high`. Give each a durable implementation-and-integration objective. Bound the pool by independent ready work and configured agent, pane, and nesting limits; reserve capacity for one large reviewer per worker. Do not raise limits automatically. If required nesting or review capacity is unavailable, report the blocker rather than silently weakening review.

Keep dispatch rolling beyond the initial cohort. Whenever an implementer's issue is integrated, validated, resolved, and explicitly confirmed complete, refresh issue states and spawn additional implementation agents for remaining independent, dependency-ready issues as capacity permits. Give every new implementer the same complete implementation, worktree isolation, validation, reviewer ownership, MMP coordination, and integration-barrier instructions as the initial implementers, plus its own issue-specific assignment and durable objective. Count all live workers and reviewers against the limits; reuse eligible idle workers when spawning would exceed capacity. Do not stop after the first cohort or wait for all implementers to finish before dispatching more ready work.

Create a unique branch and worktree per assigned issue from the latest integrated main-branch commit. Use collision-free paths within allowed write scopes. Assign each worker its absolute worktree path and require all repository commands and edits to target that worktree, with its applicable repository instructions. No worker may edit another worker's worktree or the main integration checkout.

Mark selected open issues `in-progress` before dispatch. Send the issue details, dependencies, relevant existing changes, worktree and branch, base commit, ownership boundaries, validation requirements, and coordinator identity. Workers return concise notes; the coordinator maintains issue notes covering the problem, fix surface, checklist, validation, review, and integration status.

## Implementation and independent review

Require each worker to inspect the relevant code, tests, docs, and contracts; make a concrete plan; implement the complete fix; add focused regression coverage when feasible; and run required validation in its worktree. Keep commits limited to the assigned issue. Report unrelated defects to the coordinator for separate issue creation without abandoning the active issue.

Each implementation agent must itself spawn and own one reusable review subagent with `role: explorer`, `lifetime: persistent`, `size: large`, and `reasoning_effort: high`. Give it a review-only objective: no implementation, edits, issue management, merges, or delegation. Discover its identity with `list_agents` and reuse it for repairs and later issues.

After implementation and validation, the worker sends its reviewer an MMP assignment containing issue id, intended behavior, worktree, exact base and head commits, diff scope, and validation evidence. Require a substantive MMP reply to that worker containing issue id, reviewed head commit, verdict (`pass` or `changes-requested`), severity-ordered findings with locations, and validation gaps. Correlate replies to assignments when message ids are available. Queued delivery, local prose, and task-status notifications are not review completion.

The worker evaluates findings, repairs applicable problems, reruns validation, and requests review of the updated commit from the same reviewer. Reject stale or mismatched verdicts. Review must pass for the exact proposed changes before requesting integration.

## Integration barrier

Implementation agents are responsible for coordinating their own integration with the main agent. After passing review, send the coordinator an MMP integration request containing issue id, worktree and branch, base and exact reviewed head commits, commit list, change summary, validation evidence, and reviewer verdict. Remain assigned to that issue until the coordinator explicitly confirms integration and resolution; do not start another issue merely because implementation or review finished.

The coordinator serializes all writes to the main integration branch. Check the request against the ledger and current branch state. If the branch is stale and overlapping changes or conflicts require updates, send it back to the worker to update in its isolated worktree, validate, and obtain a fresh review. Never resolve conflicts silently in the integration checkout or accept a verdict for changed content.

Integrate the approved commits using repository conventions, without destructive history rewrites. Confirm the intended changes landed on the main branch, then run focused and repository-required integration checks there; worker validation alone is insufficient. Record the resulting main-branch commit and validation results. Keep the worker assigned through any integration failure and coordinate repair plus renewed review. Do not mark an issue resolved while its changes are unmerged or integration checks fail.

Only after passing review, successful integration, and main-branch validation may the coordinator mark the issue `resolved` and send an explicit integration-complete message with issue id, integrated commit, and validation outcome. The worker acknowledges substantive completion in its handoff, retains its reviewer for later assignments, and becomes eligible for another issue only after this confirmation. Start the next issue in a fresh worktree based on the updated main branch.

The main agent (coordinator) owns worktree cleanup after each completed issue, not just at final teardown. After the worker's completion handoff, confirm that both worker and reviewer have stopped using the old worktree before reassignment. Recheck that it is task-created, its assigned work is safely integrated into the main branch, and it has no uncommitted or untracked changes. Remove it with `git worktree remove` without force, then verify that both its directory and Git registration are gone and record cleanup in the ledger. Preserve dirty, unmerged, or still-in-use worktrees; record their paths and cleanup blockers rather than discarding work.

For a blocked issue, record the blocker, retry condition, remaining branch/worktree state, and any recovery obligation before releasing its worker. Preserve unmerged work. Deferral is not completion; never abandon partially integrated failures to take another issue. Continue dispatching unrelated workable issues.

## Coordination and finish

Discover peers with `list_agents` before `send_message`. Use direct coordinator-to-worker and worker-to-reviewer messages; no delivery-only acknowledgments or relay agents. Use `wait` as the only executable action only when a substantive peer reply is outstanding and no independent coordinator or worker work can proceed. Do not wait for shell processes, approvals, or task-status notifications.

After each resolution or deferral, refresh both issue states and replenish the implementation pool with additional agents using the same full instructions as the initial cohort, within the worker and reviewer capacity limits above; also fill eligible idle worker slots with independent ready work. Finish only when no assignment or integration remains active and fresh queries show every remaining issue deferred with a concrete external blocker or unresolved dependency.

Have each worker close its own persistent reviewer after discovering its current identity. Then discover and close the coordinator-owned persistent workers. Remove only task-created worktrees and branches whose work is safely integrated and whose worktrees are clean; preserve blocked or unmerged work and report its location.

Report resolved issues with main-branch commits, validation and review evidence, deferred issues with retry conditions, and any preserved worktrees or skipped checks.
