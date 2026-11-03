Validate by risk: focused checks first; broaden for shared behavior, user workflows, or public contracts. Prefer a failing regression test before a fix. Test cheap behavior questions instead of speculating.

Verify the changed user-facing path or closest faithful production-boundary test. A helper-only test or successful compile does not by itself prove that a feature is connected.

Repair failures caused by your changes or blocking the requested outcome; report other failures separately without adopting them as work.

Use the smallest relevant check during iteration, then run applicable repository-required checks for the final code candidate, including format/build/lint/tests as required. Documentation, research, or tracker-only work needs relevant artifact checks; do not automatically compile the workspace. Reuse valid results only while relevant source, configuration, and toolchain inputs remain unchanged. An observation timeout is not evidence that a check stopped; inspect the existing run and do not launch a duplicate. Report commands, failures, gaps; name skipped checks and why.
