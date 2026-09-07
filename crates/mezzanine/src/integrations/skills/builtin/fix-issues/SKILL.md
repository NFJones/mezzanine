---
name: fix-issues
description: Work the current project's mez issues to verified resolution, keeping concise issue plans and progress notes.
---

Query `in-progress` issues first, then `open` issues. Reuse successful results until the issue store changes. If neither query returns issues, stop.

Work one dependency-ready issue at a time. Prioritize, in order:
1. in-progress issues related to uncommitted changes;
2. other in-progress issues;
3. open issues related to uncommitted changes;
4. other open issues.

Establish whether dirty files relate to an issue with repository evidence; do not assume every uncommitted change is related. Work prerequisites before dependent issues. Mark a selected open issue `in-progress` before implementation.

For each issue, inspect enough code, tests, docs, and specifications to make a concrete plan. Record and maintain concise issue notes containing:
- problem and intended fix surface;
- implementation checklist/status;
- validation steps and results.

Use subagents as the normal workflow for nontrivial issues: have a **large** model plan the work, then spawn a **medium** model to implement it; use a **small** model only for tightly scoped, low-risk implementation. Keep the issue plan, implementation, and validation aligned with the selected issue. Once an issue plan is complete, use a **large** model to review the implementation for defects and then plan fixes for identified defects as a part of the issue. Ensure that subagents are explicitly told the scope and bounds of their respective tasks and when they should return.

Implement the complete fix and add or update focused regression coverage when feasible. Every issue plan must include validation steps; run them before resolution and record the outcome.

When an error is discovered, fix it within the active issue plan if it is related to that issue. If it is unrelated, create a new defect issue with the observed error and continue the active issue.

Update issue notes when the plan, progress, validation result, or next action changes. After successful verification, record the validation outcome and mark the issue `resolved`; do not delete it. If blocked, record the blocker.

After resolving or blocking an issue, query both states again and continue until no eligible issues remain. Do not resolve an issue until its own implementation and validation are complete.
