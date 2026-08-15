<div align="center">
<p align="center">
  <picture>
    <source
      srcset="./resources/mezzanine-combined-light.png"
      media="(prefers-color-scheme: dark)"
    />
    <source
      srcset="./resources/mezzanine-combined-dark.png"
      media="(prefers-color-scheme: light)"
    />
    <img
      src="./resources/mezzanine-combined-dark.png"
      width="850"
      alt="Mezzanine logo"
    />
  </picture>
</p>
<p align="center">
  <a href="https://github.com/NFJones/mezzanine/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/NFJones/mezzanine?style=flat-square"></a>
  <a href="https://github.com/NFJones/mezzanine/forks"><img alt="GitHub forks" src="https://img.shields.io/github/forks/NFJones/mezzanine?style=flat-square"></a>
  <a href="https://github.com/NFJones/mezzanine/issues"><img alt="GitHub issues" src="https://img.shields.io/github/issues/NFJones/mezzanine?style=flat-square"></a>
  <a href="https://github.com/NFJones/mezzanine/actions"><img alt="Build status" src="https://img.shields.io/github/actions/workflow/status/NFJones/mezzanine/ci.yml?style=flat-square"></a>
  <a href="https://deepwiki.com/NFJones/mezzanine"><img alt="Ask DeepWiki" src="https://deepwiki.com/badge.svg"></a>
</p>
</div>

***

<div align="center">
<picture>
    <img
      src="./resources/mez-demo.png"
      width="800"
      alt="Mezzanine demo"
    />
</picture>
</div>

***

Mezzanine is a terminal multiplexer with a built-in pane-local agent. Keep a
shell, logs, editor, and agent conversation together in one recoverable session
while you inspect, edit, and validate work.

## Why Mezzanine?

- **Persistent sessions:** windows, panes, detach/reattach, and copy mode.
- **Pane-local agents:** independent conversations beside the shell where work
  already lives.
- **Reviewable actions:** visible shell commands, patches, approvals, MCP calls,
  and subagent work.
- **Safety controls:** approval policy, project trust, and optional OS-level
  confinement in Linux pane environments are distinct, visible controls.

## Prerequisites

- Linux or macOS, with pseudoterminals and a POSIX-style shell.
- A Rust toolchain that supports edition 2024 when building from this repository.
- A usable `$SHELL`; Mez falls back to `/bin/sh` when it is executable.
- A provider account and supported sign-in method for model-backed agent work.

## Quick start

Install the product package:

```sh
cargo install --path crates/mezzanine --locked
```

Cargo normally installs `mez` in `~/.cargo/bin`. Ensure that directory is on
`PATH`, or invoke `~/.cargo/bin/mez` in the commands below.

Optionally create a baseline configuration, authenticate, and start Mezzanine
in a working directory. Starting a session creates the default configuration
when none exists:

```sh
mez config init
mez auth login
cd /path/to/repository
mez new
```

The default login flow opens OpenAI's browser sign-in. See
[Getting started](docs/getting-started/README.md) for other providers, API keys,
and noninteractive authentication, or the
[configuration reference](docs/configuration/reference.md) for backend options.

Press `Ctrl+A a` to open the focused pane's agent shell. Begin with a bounded
task that asks for inspection and focused validation. Press `Ctrl+A d` to detach
without normally stopping the session.

Optional configuration can inhibit idle sleep during active agent turns and
enable enhanced keyboard reporting in supported terminals. See the
[configuration reference](docs/configuration/reference.md) for platform support
and behavior.

## Everyday use

Use `mez new` to create a new session, `mez list` to discover resumable sessions,
and `mez attach` to return to one. In a running session, `Ctrl+A :` opens the
Mezzanine command prompt, `Ctrl+A ?` shows effective key bindings, and
`Ctrl+A a` toggles the agent shell.

Within an agent pane, use `/plan on`, `/plan off`, or `/plan toggle` to control
pane-local plan-only mode. The clickable `plan` status pill shows the current
mode.

On supported Bash, Zsh, and Fish prompts, Mezzanine preserves an unfinished
command while agent mode is active and restores it afterward. Unsupported or
unsafe prompt states fail closed rather than submitting or combining input.

Use `mez --help` and the [CLI reference](docs/reference-manual/cli.md) for the
current command contract. Use [Sessions and panes](docs/using-mezzanine/sessions-and-panes.md)
and [Terminal input, copy, and history](docs/using-mezzanine/terminal-input-copy-and-history.md)
for in-session work.

### Shell completion

Generate shell completions from the installed `mez` binary. For the current zsh
session, run:

```sh
source <(mez completion zsh)
```

Replace `zsh` with `bash`, `fish`, `elvish`, or `powershell` as needed. Install
the generated output according to your shell's conventions for future sessions.

## Safety at a glance

The agent works from its pane's working directory, configured guidance, and
explicit action results. It does not passively receive your terminal screen,
scrollback, or other panes.

Approval policy decides whether Mezzanine permits an action. OS confinement
separately controls what an already-permitted local shell process can access,
while web and integration actions have their own capability and approval gates.
The current confinement backend is Bubblewrap in Linux pane environments.
Other pane environments use the policy-only backend, which does not provide
OS-level isolation.

Review unfamiliar project overlays and applicable `AGENTS.md` files before
trusting their guidance. Project instructions can shape workflow but cannot
grant authority. See
[Safety, trust, and security](docs/safety-and-trust/README.md) for approval,
confinement, project-trust, and audit guidance.

## Documentation

The [Mezzanine manual](docs/README.md) is organized by task and audience:

- [Getting started](docs/getting-started/README.md)
- [Using Mezzanine](docs/using-mezzanine/README.md)
- [Agent and integrations](docs/agent/README.md)
- [Safety, trust, and security](docs/safety-and-trust/README.md)
- [Configuration](docs/configuration/README.md)
- [Operations and troubleshooting](docs/operations/README.md)
- [CLI, key, action, and terminal reference](docs/reference-manual/README.md)
- [Contributor documentation](docs/contributing/README.md)

[SPEC.md](SPEC.md) remains the normative behavior and compatibility contract.
[AGENTS.md](AGENTS.md) contains repository workflow requirements for contributors.
