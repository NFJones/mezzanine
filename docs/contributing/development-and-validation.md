# Development and validation

## Purpose

Set up a local contributor workflow and run the repository's required checks
before handing off a change.

## Prerequisites

- Rust 1.91 or newer, including `rustfmt` and `clippy`.
- `just` for the repository recipes.
- GNU `timeout` available as an executable named `timeout` on `PATH`.
- Python 3 available as `python3` for managed-shell Cargo-artifact parsing and
  profiling scripts.
- Bash, Fish, Zsh, and a POSIX `/bin/sh` for managed-shell and real-PTY checks.
  The managed-shell wrapper requires Bash at `/bin/bash` or `/usr/bin/bash`,
  Fish at `/usr/bin/fish`, `/usr/local/bin/fish`, or `/opt/homebrew/bin/fish`,
  and Zsh at `/bin/zsh`, `/usr/bin/zsh`, or `/usr/local/bin/zsh`; missing
  supported shells are errors, not silent skips.
- The repository [AGENTS.md](../../AGENTS.md), which is the authoritative
  workflow and handoff guidance.

On macOS, install Homebrew coreutils and expose its GNU executable names before
running recipes or tests:

```sh
brew install coreutils fish
export PATH="$(brew --prefix coreutils)/libexec/gnubin:$PATH"
command -v timeout
timeout --version
python3 --version
```

The managed-shell wrapper itself defaults to `gtimeout` on macOS (and accepts
`TIMEOUT_COMMAND`), but wrapping only the outer command with `gtimeout` is not
sufficient: recipes and managed-shell test commands also invoke the executable
name `timeout`. A shell alias does not make that name available to child
processes. Keep coreutils' `gnubin` directory on the inherited `PATH`. Ensure
`python3` and all supported shells above are installed too; coreutils and Fish
alone do not supply every prerequisite. Linux typically supplies GNU timeout
through coreutils; install Fish and Zsh if absent.

## Build and run

Run these commands from the workspace root:

```sh
just build                 # Debug build of all targets and features
just build-release         # Release build of all targets and features
just run --help            # Run the release mez binary with arguments
```

`just` without a recipe builds all workspace targets and features in release
mode. Use `just help` to list the available recipes. Keep generated output in
`target/` out of source changes and commits.

Pass Mez arguments directly after `just run`; the recipe already inserts
Cargo's `--` separator. For example, `just run config validate` runs
`cargo run -p mezzanine --release -- config validate`.

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

For direct test commands, use the short physical temporary directory that the
`just test` recipe selects. On macOS, inherited `TMPDIR` values can exceed
Unix-domain socket path limits, and `/tmp` is a symlink to `/private/tmp`:

```sh
canonical_tmp="$(cd /tmp && pwd -P)"
TMPDIR="$canonical_tmp" timeout 120s cargo test -p mez-agent --lib --all-features --quiet semantic_apply_patch_replace_whole_file
```

Increase the timeout when compilation or the selected suite needs more time;
120 seconds is a minimum, not the expected duration of the workspace suite.

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
| OpenAI cache-probe authorization, request shape, and redaction (fake transport) | `just test-openai-prompt-cache-probe` |

Wrap test-running recipes that do not supply their own timeout, including the
release-load and Iroh benchmark recipes, in `timeout` with a budget of at least
120 seconds; allow extra time for release compilation or a multi-run sweep.

For real X11 forwarding validation on Linux, install `Xvfb` and `xauth`, then
run `sh scripts/test-x11-forwarding.sh` with the physical `TMPDIR` above. The
script starts an authenticated temporary Xvfb server and runs the ignored
trusted/untrusted setup round-trip acceptance test with quiet Cargo output and
a 300-second timeout. Missing tools or X SECURITY support fail the check rather
than downgrading untrusted forwarding to trusted mode. Linux CI runs this
separate acceptance step; the ordinary workspace suite does not run the ignored
test.

For native Linux power-inhibition changes, the optional
`MEZ_REAL_LINUX_POWER_INHIBITION=1 just test-real-linux-power-inhibition`
qualifies the real systemd-logind and desktop ScreenSaver backend. It requires
a systemd host, `busctl`, accessible system and session D-Bus services, and
`DBUS_SESSION_BUS_ADDRESS`; WSL and missing services fail preflight rather than
substituting fake coverage. The script supplies a 120-second test timeout and
does not change idle settings.

`just probe-openai-prompt-cache` is a separate live-provider observation, not
part of the offline regression recipe or required suite. It sends two synthetic
requests only with `MEZ_OPENAI_CACHE_PROBE=1`, an environment-supplied
`OPENAI_API_KEY`, and `MEZ_OPENAI_CACHE_PROBE_MODEL`. It requires `curl` and
Python 3, uses only the canonical OpenAI Responses endpoint, and prints sanitized
cache-usage observations. Do not put credentials in command arguments or reports;
live requests require explicit authorization and may incur provider charges.

The release-load artifact is content-safe and report-only. Alongside the
multi-pane PTY/input/render workload, it records the fixture count and body
size plus p50/p95/p99 render latency for a retained large record-browser
overlay. The report never serializes record titles, metadata, or bodies. Use
identical worker counts and fixture sizes when comparing artifacts; the sample
is a regression signal, not a portable latency budget.

Fake-I/O attached-terminal routing tests use paused Tokio time to isolate
correctness from other test threads' CPU scheduling. Dedicated timeout tests
still verify the 250 ms per-operation deadline and accepted-input noncancellation.
An end-to-end loop can contain multiple awaited operations; that deadline is not
a whole-loop wall-clock SLA. Use controlled release-load measurements for
responsiveness qualification, not an oversubscribed debug test harness.

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
