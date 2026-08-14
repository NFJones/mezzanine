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
    cargo install --path crates/mezzanine

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

# Run the strict routed lifecycle acceptance with genuine Bubblewrap confinement
test-real-bubblewrap:
    test "$(uname -s)" = Linux
    timeout 120s cargo test -p mezzanine --lib --all-features --quiet -- --exact host::async_runtime::tests::services::providers::async_routed_subagent_settles_with_real_bubblewrap --ignored --nocapture

# Run the report-only cross-platform responsiveness workload in release mode.
release-load-check:
    report="${MEZ_RELEASE_LOAD_REPORT:-target/release-load/$(uname -s | tr '[:upper:]' '[:lower:]').json}"; case "$report" in /*) ;; *) report="$(pwd)/$report";; esac; mkdir -p "$(dirname "$report")"; MEZ_RELEASE_LOAD_REPORT="$report" cargo test -p mezzanine --release --lib --all-features --quiet host::async_runtime::tests::services::release_load::release_load_reports_cross_platform_pty_responsiveness -- --exact --ignored --nocapture --test-threads=1

# Clean build artifacts
clean:
    cargo clean

# List available recipes
help:
    just --list
