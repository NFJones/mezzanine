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
    cargo install --path .

# Run (release by default)
run *args:
    cargo run -p mezzanine --release -- {{args}}

# Type-check without building artifacts
check:
    cargo check --workspace --all-targets --all-features

# Format with rustfmt
fmt:
    cargo fmt --all

# Lint with clippy and deny warnings
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run tests
test:
    cargo test --workspace --all-targets --all-features

# Reject forbidden dependencies between Mezzanine workspace crates
architecture:
    python3 scripts/check-workspace-dependencies.py

# Clean build artifacts
clean:
    cargo clean

# List available recipes
help:
    just --list
