# Install Mezzanine

## Purpose

Install the `mez` executable and confirm that the local environment can start
Mezzanine sessions.

## Prerequisites

- A Unix-like system with pseudoterminals and a POSIX-style shell.
- A usable `$SHELL`; Mezzanine falls back to `/bin/sh` when it is executable.
- A Rust 2024 toolchain when building from this repository.

## Install from this repository

From the repository root, install the product package with Cargo:

```sh
cargo install --path crates/mezzanine --locked
```

Cargo usually places the executable in `~/.cargo/bin`. Ensure that directory
is on `PATH`, or invoke `~/.cargo/bin/mez` explicitly. Confirm the installed
command with `mez --version` and inspect top-level operations with `mez --help`.

Invoking `mez` without a subcommand starts a new session. Use `mez list` and
`mez attach` to return to an existing session. It also provides configuration,
authentication, MCP, issue, memory, sandbox, and snapshot commands.

## Before enabling confinement

Bubblewrap confinement requires a configured `bwrap` executable in the active
pane environment; Mezzanine does not install a privileged helper. Review its
authority, network, and approval settings before enabling it.

## Related pages

- [Authenticate a provider](authentication.md)
- [Start your first session](first-session.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Continue to [Authenticate a provider](authentication.md) before using a
model-backed agent.
