# mezzanine

The Mezzanine product package, which builds the `mez` executable: a terminal
multiplexer with a built-in pane-local agent. Keep shells, logs, an editor,
and agent conversations together in a recoverable session while you inspect,
edit, and validate work.

## Why Mezzanine?

- **Persistent sessions:** windows, panes, detach/reattach, and copy mode.
- **Pane-local agents:** independent conversations beside the shell where work
  already lives.
- **Reviewable actions:** visible shell commands, patches, approvals, MCP
  calls, and subagent work.
- **Explicit safety controls:** approval policy, project trust, and optional
  OS-level confinement remain distinct controls.

This package composes the four lower workspace libraries into the application.
Its library API is deliberately narrow; it is not a compatibility facade for
the lower crates.

## Prerequisites

- Linux or macOS, with pseudoterminals and a POSIX-style shell.
- Rust 1.91 or newer when building from this repository.
- A usable `$SHELL`; Mez falls back to `/bin/sh` when it is executable.
- A provider account and supported sign-in method, or a configured compatible
  local backend, for model-backed agent work.

## Quick start

Clone the repository and install this package from the workspace root:

```sh
git clone https://github.com/NFJones/mezzanine.git
cd mezzanine
cargo install --path crates/mezzanine --locked
```

Cargo normally installs `mez` in `~/.cargo/bin`; ensure that directory is on
`PATH`. From a checkout, `just install` also supports a read-only default Cargo
install root by falling back to `target/mez-install/bin`.

Optionally initialize configuration, authenticate, and start a session:

```sh
mez config init
mez auth login
cd /path/to/repository
mez new
```

Starting a session creates the default configuration when none exists. With
an interactive terminal, the default OpenAI login flow prefers browser sign-in.
See [Getting started](../../docs/getting-started/README.md) for other providers,
API keys, and noninteractive authentication.

## Everyday use

- `mez new` creates a session; `mez list` discovers resumable sessions;
  `mez attach` returns to one.
- `Ctrl+A a` opens or closes the focused pane's agent shell.
- `Ctrl+A d` detaches without normally stopping the session.
- `Ctrl+A :` opens the command prompt; `Ctrl+A ?` shows effective key bindings.
- `mez --help` describes the CLI; `mez completion <shell>` generates shell
  completions.

The [agent guide](../../docs/agent/README.md) covers plan-only mode and
integrations. The [agent-shell guide](../../docs/using-mezzanine/agent-shell.md)
explains native and pane execution, including supported SSH and container
shell workflows. For persistent service deployment, see
[Persistent multi-session host](../../docs/operations/persistent-host.md).

## Safety at a glance

The agent receives its pane's working directory, configured guidance, and
explicit action results. It does not passively receive your terminal screen,
scrollback, or other panes.

Approval policy controls whether an action is permitted. Optional OS-level
confinement separately limits permitted local shell processes: Bubblewrap on
Linux uses private namespaces, while Seatbelt on macOS applies operation-level
policy in the visible host namespace and is not namespace-equivalent isolation.
Web and integration actions have their own capability and approval gates.

Review unfamiliar project overlays and applicable `AGENTS.md` files before
trusting their guidance. Project instructions cannot grant authority. See
[Safety, trust, and security](../../docs/safety-and-trust/README.md).

## Workspace architecture

| Crate | Responsibility |
| --- | --- |
| [`mez-core`](../mez-core/README.md) | Shared identifiers and validation invariants |
| [`mez-terminal`](../mez-terminal/README.md) | One-terminal parsing, screen, history, and compatibility |
| [`mez-mux`](../mez-mux/README.md) | Multiplexer domain, PTY, layout, input, and presentation |
| [`mez-agent`](../mez-agent/README.md) | Agent harness, provider protocols, and deterministic policy |
| `mezzanine` | CLI, configuration, control, security, storage, integrations, host I/O, UI, and runtime composition |

[`src/main.rs`](src/main.rs) is the thin binary entry point.
[`src/lib.rs`](src/lib.rs) exposes product bootstrap functions, error types,
and the intentionally supported control-client framing helpers. Import
reusable domain contracts directly from their owning lower crate.

## Development and validation

From the repository root:

```sh
cargo build -p mezzanine --locked
cargo run -p mezzanine -- --help
just check
just fmt
just clippy
timeout 300s just test
```

`just test` runs the workspace's targets and features with quiet Cargo output
and platform-specific temporary-directory handling. Test commands require a
`timeout` executable; increase the limit when needed. Use the
[development guide](../../docs/contributing/development-and-validation.md) for
focused testing and platform-specific acceptance checks. Changes must preserve
both Linux and macOS compatibility.

## Documentation

- [Repository overview](../../README.md)
- [Mezzanine manual](../../docs/README.md)
- [Getting started](../../docs/getting-started/README.md)
- [Configuration reference](../../docs/configuration/reference.md)
- [CLI reference](../../docs/reference-manual/cli.md)
- [Operations and troubleshooting](../../docs/operations/README.md)
- [Contributor guide](../../docs/contributing/README.md)

[`SPEC.md`](../../SPEC.md) is the normative behavior and compatibility contract.
[`AGENTS.md`](../../AGENTS.md) contains repository workflow requirements.

## License

Licensed under the [Apache License 2.0](../../COPYING).
