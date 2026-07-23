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

# Run tests
test:
    cargo test --workspace --all-targets --all-features

# Clean build artifacts
clean:
    cargo clean

# List available recipes
help:
    just --list
