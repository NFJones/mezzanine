# Install Mezzanine

## Purpose

Install the `mez` executable and confirm that the local environment can start
Mezzanine sessions.

## Prerequisites

- Linux or macOS, with pseudoterminals and a POSIX-style shell.
- A usable `$SHELL`; Mezzanine falls back to `/bin/sh` when it is executable.
- Rust 1.91 or newer when building from this repository.

## Install from this repository

From the repository root, install the product package with Cargo:

```sh
cargo install --path crates/mezzanine --locked
```

Cargo usually places the executable in `~/.cargo/bin`. Ensure that directory
is on `PATH`, or invoke `~/.cargo/bin/mez` explicitly. Confirm the installed
command with `mez --version` and inspect top-level operations with `mez --help`.

Without a subcommand, `mez` attaches to the first session that accepts a
primary client; when none is available, it creates a session. Use `mez new`
when creating a session is intentional. Use `mez list` and `mez attach` to
inspect and select existing sessions. Use the [CLI
reference](../reference-manual/cli.md) or `mez --help` for the complete command
tree, including configuration, authentication, persistent-host, remote-pairing,
integration, and diagnostic commands.

## Before enabling confinement

Bubblewrap confinement requires a Linux pane environment and a configured
`bwrap` executable there; Mezzanine does not install a privileged helper. Other
pane environments use `policy-only`, which does not provide OS-level isolation.
Review Bubblewrap authority, network, and approval settings before enabling it.

## Related pages

- [Authenticate a provider](authentication.md)
- [Start your first session](first-session.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Continue to [Authenticate a provider](authentication.md) before using a
model-backed agent.
