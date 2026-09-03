# Default recipe builds in release mode
default:
    cargo build --workspace --all-targets --all-features --release

# Build (debug)
build:
    cargo build --workspace --all-targets --all-features

# Build (release)
build-release:
    cargo build --workspace --all-targets --all-features --release

# Install mez
install:
    if [ -n "${CARGO_INSTALL_ROOT:-}" ] || [ -w "${CARGO_HOME:-$HOME/.cargo}" ]; then cargo install --path crates/mezzanine --locked; else install_root="$(pwd)/target/mez-install"; printf '%s\n' "Cargo install root is read-only; installing mez under $install_root/bin"; CARGO_INSTALL_ROOT="$install_root" cargo install --path crates/mezzanine --locked; fi

# Run (release by default)
run *args:
    RUST_BACKTRACE=1 cargo run -p mezzanine --release -- {{args}}

# Type-check without building artifacts
check:
    cargo check --workspace --all-targets --all-features

# Format with rustfmt
fmt:
    cargo fmt --all

# Lint with clippy and deny warnings
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run tests below the short physical system temporary directory. macOS exposes
# /tmp through /private/tmp while its inherited TMPDIR is both symlink-bearing
# and too long for test-specific Unix-domain socket names.
test:
    canonical_tmp="$(cd /tmp && pwd -P)"; if [ "$(uname -s)" = Darwin ]; then TMPDIR="$canonical_tmp" cargo test --workspace --all-targets --all-features --no-fail-fast --quiet -- --test-threads=1; else TMPDIR="$canonical_tmp" cargo test --workspace --all-targets --all-features --no-fail-fast --quiet; fi

# Require real Bash, Fish, and Zsh and run the focused managed-shell acceptance
# suite. macOS additionally exercises acknowledged PTY pacing and the large
# semantic-patch path through its physical platform implementation.
test-managed-shells:
    canonical_tmp="$(cd /tmp && pwd -P)"; TMPDIR="$canonical_tmp" sh scripts/test-managed-shell-reliability.sh

# Build test binaries once, execute their libtest harnesses serially, and write
# a report of slow tests under target/. Tests above the conservative critical
# budget fail this explicitly requested profiling run, not ordinary test runs.
profile-slow-tests *args:
    python3 scripts/profile-slow-tests.py {{args}}

# Run the strict routed lifecycle acceptance with genuine Bubblewrap confinement
test-real-bubblewrap:
    test "$(uname -s)" = Linux
    timeout 120s cargo test -p mezzanine --lib --all-features --quiet -- --exact host::async_runtime::tests::services::providers::async_routed_subagent_settles_with_real_bubblewrap --ignored --nocapture

# Run the complete macOS Seatbelt compiler, pane/native runtime, cleanup,
# recovery, and product-binary acceptance surface serially.
test-real-seatbelt:
    test "$(uname -s)" = Darwin
    test -x /usr/bin/sandbox-exec
    canonical_tmp="$(cd /tmp && pwd -P)"; TMPDIR="$canonical_tmp" timeout 300s cargo test -p mezzanine --lib --all-features --quiet seatbelt -- --test-threads=1
    canonical_tmp="$(cd /tmp && pwd -P)"; TMPDIR="$canonical_tmp" timeout 120s cargo test -p mezzanine --test foreground_cli --all-features --quiet real_seatbelt_ -- --nocapture --test-threads=1

# Run the report-only cross-platform responsiveness workload in release mode.
release-load-check:
    report="${MEZ_RELEASE_LOAD_REPORT:-target/release-load/$(uname -s | tr '[:upper:]' '[:lower:]').json}"; case "$report" in /*) ;; *) report="$(pwd)/$report";; esac; mkdir -p "$(dirname "$report")"; MEZ_RELEASE_LOAD_REPORT="$report" MEZ_RELEASE_LOAD_WORKERS="${MEZ_RELEASE_LOAD_WORKERS:-2}" cargo test -p mezzanine --release --lib --all-features --quiet host::async_runtime::tests::services::release_load::release_load_reports_cross_platform_pty_responsiveness -- --exact --ignored --nocapture --test-threads=1

# Run the reproducible application-frame compression benchmark in release mode.
iroh-compression-bench:
    report="${MEZ_IROH_COMPRESSION_BENCH_REPORT:-target/iroh-compression-bench.json}"; case "$report" in /*) ;; *) report="$(pwd)/$report";; esac; mkdir -p "$(dirname "$report")"; MEZ_IROH_COMPRESSION_BENCH_REPORT="$report" cargo test -p mezzanine --release --lib --all-features --quiet runtime::iroh_compression::tests::iroh_compression_release_benchmark -- --exact --ignored --nocapture --test-threads=1

# Run the content-safe Iroh render-update and RTT-model benchmark in release mode.
iroh-render-bench:
    report="${MEZ_IROH_RENDER_BENCH_REPORT:-target/iroh-render-bench.json}"; case "$report" in /*) ;; *) report="$(pwd)/$report";; esac; mkdir -p "$(dirname "$report")"; MEZ_IROH_RENDER_BENCH_REPORT="$report" cargo test -p mezzanine --release --lib --all-features --quiet runtime::iroh::tests::iroh_render_update_release_benchmark -- --exact --ignored --nocapture --test-threads=1

# Compare product-shaped release workloads across explicit Tokio worker counts.
release-load-sweep:
    for workers in ${MEZ_RELEASE_LOAD_WORKER_SWEEP:-1 2 4}; do MEZ_RELEASE_LOAD_WORKERS="$workers" MEZ_RELEASE_LOAD_REPORT="target/release-load/$(uname -s | tr '[:upper:]' '[:lower:]')-workers-$workers.json" just release-load-check; done

# Clean build artifacts
clean:
    cargo clean

# List available recipes
help:
    just --list
