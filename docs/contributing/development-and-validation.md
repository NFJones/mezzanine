# Development and validation

## Purpose

Set up a local contributor workflow and run the repository's required checks
before handing off a change.

## Prerequisites

- Rust 1.91 or newer, including `rustfmt` and `clippy`.
- `just` for the repository recipes.
- The repository [AGENTS.md](../../AGENTS.md), which is the authoritative
  workflow and handoff guidance.

## Build and run

Run these commands from the workspace root:

```sh
just build                 # Debug build of all targets and features
just build-release         # Release build of all targets and features
just run -- --help         # Run the release mez binary with arguments
```

`just` without a recipe builds all workspace targets and features in release
mode. Use `just help` to list the available recipes. Keep generated output in
`target/` out of source changes and commits.

## Validate changes

Use the narrowest check while developing, then run the complete required set
before handoff. The commands below are repository recipes; run them from the
workspace root:

```sh
just fmt
just check
just clippy
timeout 120s just test
```

`just fmt` applies Rust formatting. `just check` type-checks all targets and
features. `just clippy` denies warnings across the workspace. `just test`
runs all targets and features with Cargo's quiet output; the timeout makes a
hang visible. Use a timeout of at least 120 seconds for every direct test
command as well. `just test` already supplies Cargo's `--quiet` option; retain
that option when running a direct `cargo test` command.

The optional `just test-real-bubblewrap` acceptance test requires Linux and a
working Bubblewrap environment. Run it when a change affects the real
confinement path. `just test-real-seatbelt` requires macOS and executable
`/usr/bin/sandbox-exec`; it runs the complete Seatbelt-filtered compiler,
pane/native runtime, cleanup, recovery, and product-binary capability-probe and
workload-launcher acceptance surface serially. These backend checks supplement
rather than replace the required workspace suite.

Use the repository's focused recipes for affected subsystems before the full
suite:

| Area | Focused recipe |
| --- | --- |
| Managed Bash, Fish, Zsh, or POSIX-shell startup | `just test-managed-shells` |
| Slow-test ownership or async responsiveness | `just profile-slow-tests` |
| Release-mode load and latency | `just release-load-check` or `just release-load-sweep` |
| Iroh compression behavior or performance | `just iroh-compression-bench` |
| Iroh v3 pushed-render behavior or RTT modeling | `just iroh-render-bench` |

Run platform-specific shell and PTY changes on both Linux and macOS when
available. To reproduce the macOS CI shape, run the full test suite serially.

The managed-shell recipe invokes
`scripts/test-managed-shell-reliability.sh`. The wrapper builds each library
test binary once with a separate
900-second budget, then runs its harness with a 300-second budget per suite
(600 seconds for the macOS large semantic-patch case). It prints phase names
and budgets so compilation timeouts cannot be mistaken for hung tests. Override
these limits with `MANAGED_SHELL_BUILD_TIMEOUT`, `MANAGED_SHELL_SUITE_TIMEOUT`,
and `MANAGED_SHELL_LONG_SUITE_TIMEOUT`. Run `timeout 120s sh
scripts/test-managed-shell-reliability-test.sh` for wrapper regression coverage;
the real supported shells must be installed for this check too. CI additionally
bounds macOS workspace tests to 15 minutes, the managed-shell step to 45 minutes,
and release-load compilation plus execution to 30 minutes.

## Change discipline

Keep a change in its subsystem owner, add focused happy-path and relevant
failure or edge coverage, and update user documentation or configuration
examples when behavior changes. Do not add compatibility shims unless the task
requires them. Treat `SPEC.md` as the normative contract and update it when a
behavioral contract changes.

Before handoff, review the diff and report commands actually run, their
outcomes, and any skipped validation. Commit coherent sequence points with an
informative imperative message; do not stage or commit material in
`docs/reference/`.

## Related pages

- [Workspace architecture](architecture.md)
- [AGENTS.md](../../AGENTS.md)
- [SPEC.md](../../SPEC.md)

## Next step

Return to [the manual](../README.md) for product documentation, or follow
[AGENTS.md](../../AGENTS.md) to implement and hand off a repository change.
