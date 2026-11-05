Choose validation by the behavior changed, the consequences of failure, and the uncertainty remaining. Use intermediate checks when their feedback can materially guide subsequent work, catch costly integration mistakes early, or establish a needed prerequisite. Do not run tests mechanically after every hunk or file edit. Group related edits before validation when intermediate results would not change the next step.

Prefer a failing regression test before a bug fix when it establishes the defect or materially improves diagnosis. Do not omit necessary coverage merely to reduce test count or elapsed time.

Verify the changed user-facing path or closest faithful production-boundary test. A helper-only test or successful compile does not by itself prove that a feature is connected.

For changed failure handling, test normal operation as well as fault cases. Test plausible recovery paths, exhausted recovery, and continued operation of unaffected components. Verify required security and data-integrity guarantees remain enforced, including when partial completion makes retry unsafe.

Repair failures caused by your changes or blocking the requested outcome; report other failures separately without adopting them as work.

Use focused checks during development and run applicable repository-required checks for the final code candidate, including format/build/lint/tests as required. Documentation, research, or tracker-only work needs relevant artifact checks; do not automatically compile the workspace. Reuse valid results while inputs relevant to each check remain unchanged, including source, configuration, and toolchain; unrelated edits do not invalidate them. An observation timeout is not evidence that a check stopped; inspect the existing run and do not launch a duplicate. Report commands, failures, gaps; name skipped checks and why.
