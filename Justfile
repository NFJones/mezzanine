# Default recipe builds in release mode
default:
    cargo build --all-targets --all-features --release

# Build (debug)
build:
    cargo build --all-targets --all-features

# Build (release)
build-release:
    cargo build --all-targets --all-features --release

# Install mez
install:
    cargo install --path .

# Run (release by default)
run *args:
    cargo run --release -- {{args}}

# Type-check without building artifacts
check:
    cargo check --all-targets --all-features

# Format with rustfmt
fmt:
    cargo fmt --all

# Lint with clippy and deny warnings
clippy:
    cargo clippy --all-targets --all-features -- -D warnings

# Run tests
test:
    cargo test --all-targets --all-features

# Clean build artifacts
clean:
    cargo clean

# List available recipes
help:
    just --list
