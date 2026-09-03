# Install Mezzanine

## Purpose

Install the `mez` executable and confirm that the local environment can start
Mezzanine sessions.

## Prerequisites

- Linux or macOS, with pseudoterminals and a POSIX-style shell.
- A usable `$SHELL`; Mezzanine falls back to `/bin/sh` when it is executable.
- Rust 1.91 or newer when building from this repository.

## Install from this repository

Clone the repository, then install the product package from its root with
Cargo. If you already have a checkout, start in that checkout instead:

```sh
git clone https://github.com/NFJones/mezzanine.git
cd mezzanine
cargo install --path crates/mezzanine --locked
```

Cargo usually places the executable in `~/.cargo/bin`. Ensure that directory
is on `PATH`, or invoke `~/.cargo/bin/mez` explicitly. Confirm the installed
command with `mez --version` and inspect top-level operations with `mez --help`.

The repository's `just install` recipe performs the same locked installation.
When Cargo's default install root is read-only, the recipe installs under
`target/mez-install/bin` instead and prints that destination. Add the printed
directory to `PATH` or invoke its `mez` executable directly.

Without a subcommand, `mez` attaches to the first session that accepts a
primary client; when none is available, it creates a session. Use `mez new`
when creating a session is intentional. Use `mez list` and `mez attach` to
inspect and select existing sessions. Use the [CLI
reference](../reference-manual/cli.md) or `mez --help` for the complete command
tree, including configuration, authentication, persistent-host, remote-pairing,
integration, and diagnostic commands.

## Update or remove the installation

To update a source installation, update the checkout and reinstall it. Use
`--force` when the package version has not changed but the source has:

```sh
git pull --ff-only
cargo install --path crates/mezzanine --locked --force
```

Review local changes before pulling in a development checkout. To remove the
Cargo-installed executable, run:

```sh
cargo uninstall mezzanine
```

## Before enabling confinement

Linux Bubblewrap confinement requires executable `/usr/bin/bwrap`. macOS
Seatbelt confinement requires executable `/usr/bin/sandbox-exec`; Apple
deprecates this command/profile interface, so verify it on every supported
macOS release. Mezzanine installs no privileged helper. Missing fixed
executables select `policy-only` for new configuration, and explicitly
configured backends fail closed rather than falling back. Review filesystem,
network, approval, and backend-specific namespace semantics before enabling
confinement.

## Related pages

- [Authenticate a provider](authentication.md)
- [Start your first session](first-session.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Continue to [Authenticate a provider](authentication.md) before using a
model-backed agent.
