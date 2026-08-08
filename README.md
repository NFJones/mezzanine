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

Mezzanine is a terminal multiplexer with a built-in pane-local agent. Use it
when you want to keep a shell, logs, editor, and agent conversation in one
recoverable session while you inspect, edit, and validate work.

Start here: [Why Mezzanine?](#why-mezzanine), [Prerequisites](#prerequisites),
and [Quick Start](#quick-start).
Look up common tasks: [Everyday Use](#everyday-use),
[Advanced Tasks](#advanced-tasks), [CLI Cheat Sheet](#cli-cheat-sheet), and
[Configuration Quick Reference](#configuration-quick-reference).
Go deeper: [Documentation Guide](#documentation-guide).
Contributing in this repository? See [Contributor Notes](#contributor-notes).

***

- [Why Mezzanine?](#why-mezzanine)
- [Prerequisites](#prerequisites)
- [Quick Start](#quick-start)
  - [1. Install `mez`](#1-install-mez)
  - [2. Create config and authenticate](#2-create-config-and-authenticate)
  - [3. Start Mezzanine inside a repository](#3-start-mezzanine-inside-a-repository)
  - [4. Open the agent shell in the focused pane](#4-open-the-agent-shell-in-the-focused-pane)
  - [5. Try a bounded first task](#5-try-a-bounded-first-task)
- [Everyday Use](#everyday-use)
  - [Start or attach to a session](#start-or-attach-to-a-session)
  - [Work in the multiplexer](#work-in-the-multiplexer)
  - [Use the agent shell](#use-the-agent-shell)
  - [Project context](#project-context)
- [Provider Support](#provider-support)
- [Agent Model and Safety](#agent-model-and-safety)
- [Advanced Tasks](#advanced-tasks)
- [What Persists Across the Session](#what-persists-across-the-session)
- [CLI Cheat Sheet](#cli-cheat-sheet)
- [Configuration Quick Reference](#configuration-quick-reference)
- [Documentation Guide](#documentation-guide)
- [FAQ](#faq)
  - [Does the agent automatically see my terminal screen?](#does-the-agent-automatically-see-my-terminal-screen)
  - [Where should API keys go?](#where-should-api-keys-go)
  - [Can I configure a different shell executable?](#can-i-configure-a-different-shell-executable)
  - [Why do status glyphs shift pane text?](#why-do-status-glyphs-shift-pane-text)
  - [How do project instructions work?](#how-do-project-instructions-work)
  - [How do project config overlays become trusted?](#how-do-project-config-overlays-become-trusted)
  - [What happens when a command needs approval?](#what-happens-when-a-command-needs-approval)
  - [Can I use more than one agent at once?](#can-i-use-more-than-one-agent-at-once)
  - [How do I run Mezzanine for automation?](#how-do-i-run-mezzanine-for-automation)
- [Contributor Notes](#contributor-notes)

## Why Mezzanine?

Mezzanine is for terminal-first development when you want project-local
context, persistent sessions, and agent help without splitting the work across
multiple tools.

It is most useful when you want all of the following in one tool:

- **Persistent terminal sessions** with windows, panes, detach, reattach, and
  copy mode.
- **Pane-local agent context** so one pane can debug a test while another stays
  on logs, a shell, or an editor.
- **Agent help in the shell environments you use** including local
  shells, containers, SSH sessions, and other command environments already open
  in a pane.
- **Explicit, reviewable actions** for shell commands, patches, approvals, MCP,
  and subagent work.
- **Built-in approval and policy controls** for shell, network, destructive,
  and other actions.
- **Conversation and session state** that survive prompt hide/show, client
  detach, and session reattach.

If you mainly want a traditional shell multiplexer, use Mezzanine as a
multiplexer. If you mainly want a coding agent, open the agent shell in the
pane where the work already lives. Then, jump back into the shell to inspect
the agent's work for yourself.

## Prerequisites

Before the first command, make sure you have:

- A Unix-like operating system with pseudoterminals and POSIX-style shells.
- A Rust 2024 toolchain if you are building from source.
- A usable `$SHELL`; otherwise Mezzanine falls back to `/bin/sh` when it is
  executable.
- Provider credentials for model-backed agent work. The generated defaults use
  the built-in OpenAI provider profile.
- A repository or other working directory you want to operate on. A repository
  gives the best first-run experience.

## Quick Start

Use this path on a clean machine.

### 1. Install `mez`

```sh
cargo install --path crates/mezzanine --locked
```

This installs `mez` into Cargo's bin directory, typically `~/.cargo/bin`. If
that directory is not already on your `PATH`, run `~/.cargo/bin/mez` in the
steps below.

Bubblewrap confinement requires only the configured `bwrap` executable in the
active pane environment. No privileged Mezzanine helper is installed.

### 2. Create config and authenticate

```sh
mez config init
mez auth login
```

Existing primary config files are migrated on launch to the current schema
version before Mezzanine validates them. Newer config schema versions than the
running binary understands are rejected.

If you are using an API key for Anthropic or another API-key-backed provider instead of OpenAI browser auth, select the provider explicitly:

```sh
mez auth login --provider anthropic --api-key
```

For noninteractive setup, add `--api-key-file PATH`.

### 3. Start Mezzanine inside a repository

```sh
cd /path/to/repository
mez
```

On first launch you should see a Mezzanine session with a focused pane running
in that repository. If you want to leave the session, press `Ctrl+A d` to
detach or close the client normally.

### 4. Open the agent shell in the focused pane

Press `Ctrl+A a`.

The prompt is pane-local, so your other panes and normal multiplexer navigation
still work while the agent shell is open.

### 5. Try a bounded first task

Ask the pane agent for one small repo-local task such as:

> Read this crate, find the most relevant failing or risky area, explain it
> briefly, then propose the smallest safe fix.
> Start with a task that mostly needs local reads and, at most, one or two
> focused commands.

## Everyday Use

Use this section when you already know the basics and want the common paths in
one place.

### Start or attach to a session

```sh
mez          # default session behavior
mez new      # create a new session
mez list     # list resumable sessions
mez attach   # attach to a resumable session
```

Foreground service mode is available when you want a daemon without
immediately attaching a primary client:

```sh
mez serve
mez attach SESSION_ID
```

Use `-S <socket-path>` to select an explicit control socket or `-L <name>` to
select a named socket under the runtime directory. Add `--json` to CLI commands
when scripting. Detached daemon stderr and panic diagnostics are retained in the private
`<control-socket>.diagnostics.log` file beside the selected control socket.

### Work in the multiplexer

Default workflow keys follow conventional multiplexer placement:

| Key                         | Action                                 |
| --------------------------- | -------------------------------------- |
| `Ctrl+A :`                  | Open the Mezzanine command prompt.     |
| `Ctrl+A ?`                  | List key bindings.                     |
| `Ctrl+A d`                  | Detach the primary client.             |
| `Ctrl+A c`                  | Create a window.                       |
| `Ctrl+A %`                  | Split vertically.                      |
| `Ctrl+A "`                  | Split horizontally.                    |
| `Ctrl+A Up/Down/Left/Right` | Focus a pane by direction.             |
| `Ctrl+A n` / `Ctrl+A p`     | Next or previous window.               |
| `Ctrl+A [`                  | Enter copy mode.                       |
| `Ctrl+A ]`                  | Paste the latest buffer.               |
| `Ctrl+A a`                  | Toggle the focused pane's agent shell. |
| `Ctrl+A C`                  | Create a new window group.             |
| `Ctrl+A (` / `Ctrl+A )`     | Previous or next group.                |

With mouse support enabled, dragging selects pane text and double-clicking
copies the surrounding readline-style word into the `mouse` paste buffer and
host clipboard when clipboard integration is available. Drag selection over a
full-screen alternate-screen pane copies the visible pane text without adding it
to scrollback or default agent context.

Terminal compatibility is tracked as a bounded implemented subset rather than a
blanket xterm claim. Consult [SPEC.md](SPEC.md) for the normative compatibility
requirements.

The Mezzanine command prompt accepts commands such as `new-window`,
`split-window`, `select-pane`, `rename-pane`, `synchronize-panes`, `set-theme`,
`list-keys`, `show-options`, and `exit`. Commands entered there are parsed
by Mezzanine, not by the pane shell.

Command output shown in the pager supports `/` text search. Submit a query to
jump to the next match; submit `/` with an empty query to repeat the last search,
wrapping to the top when no later match exists.

The `/show-context`, `/show-issues`, `/show-memories`, and
`/list-personalities` record browsers render
their list views as tables whose left-most stable ID is the only selectable link
for each record. Arrow keys move between those ID links. `Enter` opens the
focused context, issue, or memory record; in `/list-personalities`, it selects
the focused personality for the active pane and refreshes the table in place.
The browsers keep `/` in-page search behavior.
`/show-issues --project` and `--project-glob` suggest known
project paths as Tab completions and shadow hints while retaining glob filters.
`/show-issues --kind` completes `defect` and `task`, while `/show-memories
--kind` completes supported memory kinds; either option preselects that kind in
the opened browser's kind picker.
`/plan on`, `/plan off`, and `/plan toggle` control a pane-local plan-only
mode. While enabled, the next prompt receives a plan-only instruction and the
pane's effective sandbox authority has no write scopes.
They add browser-specific keys: `k` opens a kind dropdown selector,
`p` opens a project/scope filter prompt, `x` opens a database-backed text filter
prompt, `s` opens a save-to-file prompt, `Enter` opens the focused record, and
`Esc` closes a prompt, returns from detail to the list, or exits the list view.
List views use the available overlay body width, including table-backed lists;
record details reflow to the smaller of that width and
`terminal.agent_wrap_column_cap`. Pager counts follow the resulting physical
rows. Full-row copy and save operations continue to use the raw Markdown rather
than visual continuation rows.

### Use the agent shell

Press `Ctrl+A a` in a pane and type a request. The agent works from the focused
pane's working directory, conversation state, and runtime settings.

Use `Ctrl+V` to paste host clipboard text into the editable agent prompt.
Multiline text, including blank lines and whitespace, stays intact until you
edit it and press Enter to submit one request.

Agent-mode logs and rendered transcript entries wrap to the active pane width,
capped at `terminal.agent_wrap_column_cap` display columns (120 by default), so
persisted and replayed transcript rows remain bounded on wide terminals.

Completed agent Markdown can render fenced `mermaid` diagrams as compact,
terminal-native Unicode rows whose foreground styling follows the active Mez
theme. Unsupported, malformed, oversized, overwide, or excess diagrams remain
literal fenced source. Copy and persisted transcript content always retain the
original Markdown rather than rendered diagram glyphs or terminal controls.

Common slash commands are `/help`, `/model`, `/approval`, `/new`, and
`/resume`. Use `/status` to inspect the active pane's session and token usage.
For the complete command, skill, and macro reference, see
[Agent skills and commands](docs/agent-skills-and-commands.md).

### Project context

- Put project-specific agent instructions in `AGENTS.md`.
- Put project config overlays under `.mezzanine/config.toml` when needed.
- Project overlays are trusted per project root. Until trusted, behavior that
  depends on the overlay is blocked or skipped with diagnostics.
- Inspect trust state with `mez sandbox trust list`; trust, reject, or revoke
  project roots through `mez sandbox trust ...`.
## Provider Support
Mezzanine natively supports OpenAI, OpenAI-compatible APIs, DeepSeek, and
Anthropic. Provider capabilities vary by API; select and authenticate a
provider with `mez auth login` and inspect the active model with `/model`.

For the current compatibility contract, see [SPEC.md](SPEC.md).

## Agent Model and Safety

- The agent can read repo files, run bounded shell commands, apply patches,
  call configured MCP tools, and delegate scoped work to subagents.
- The agent is pane-local: it works from the focused pane rather than from a
  hidden global view of your terminal.
- The agent does not passively receive your full screen, scrollback, or other
  panes by default. It sees explicit prompts, configured instructions,
  conversation context, and explicit action results.
- Shell, network, destructive, configuration, and some MCP actions may require
  approval depending on the active runtime mode.
- Approval policy and OS confinement are separate: `policy-only` records and
  gates actions but does not confine host processes, while Bubblewrap enforces
  configured filesystem and network boundaries. `host-access` runs outside the
  sandbox and is reserved for the primary user.
- Use `/approval`, `/permissions`, and `/sandbox` to inspect or adjust the
  active pane's policy. Project overlays remain pending until explicitly
  trusted.

See [Sandbox mechanism](docs/sandbox-mechanism.md) for confinement, authority,
network, managed-home, profile, cache, and environment behavior. See
[SPEC.md](SPEC.md) for normative security and approval requirements.

## Advanced Tasks
Ask the pane-local agent to debug a focused failure, make a small change, or
delegate a bounded investigation. Review requested approvals in the primary
client before allowing them. Before trusting a project overlay, review its
`.mezzanine/config.toml` and `AGENTS.md`, then inspect pending trust with
`mez sandbox trust list`.

See [Agent skills and commands](docs/agent-skills-and-commands.md) and
[Sandbox mechanism](docs/sandbox-mechanism.md) for detailed workflows.

## What Persists Across the Session

- Session layout, windows, panes, and pane history persist according to the
  active session and history settings.
- Pane-local agent conversation state can survive prompt hide/show, detach, and
  reattach flows.
- Live agent settings such as the selected model, approval mode, and log level
  can remain associated with the pane agent session.
- Project trust decisions and persisted configuration changes remain in the
  relevant config or trust store, not just the live client process.
- Persisted state does not bypass approval or trust checks.

## CLI Cheat Sheet

```text
mez [--json] <command> [options]
```

Common commands are `mez`, `mez new`, `mez list`, `mez attach`, `mez config
init`, `mez auth login`, and `mez sandbox trust list`. Add `--json` when
scripting, `-S <socket-path>` for an explicit control socket, or `-L <name>`
for a named socket.

Use `mez auth status` for shareable authentication diagnostics. Credentials and
tokens are managed by `mez auth`, not configuration files. See
[Agent skills and commands](docs/agent-skills-and-commands.md) and
[SPEC.md](SPEC.md) for the complete CLI, authentication, and MCP reference.

## Configuration Quick Reference

Use the dedicated reference for generated defaults, supported fields, and layer
behavior:

- [Configuration reference](docs/configuration-reference.md)
- [Example config](docs/examples/config.toml)
- [SPEC.md Section 8](SPEC.md#8-configuration)

Common tasks:

| Task                              | Command or path                    |
| --------------------------------- | ---------------------------------- |
| Create a starter config           | `mez config init`                  |
| Validate the current config       | `mez config validate`              |
| Inspect the effective config      | `mez config get`                   |
| Show the built-in defaults        | `mez config default`               |
| Change the active theme           | `mez config set theme.active nord` |
| Inspect trust state               | `mez sandbox trust list`           |
| Trust a project root              | `mez sandbox trust add PATH`       |
| Change model selection at runtime | `/model`                           |
| Toggle supported thinking mode    | `/thinking`                        |
| Change pane-subtree approval mode | `/approval`                        |

Credentials belong in `mez auth`, not in config files.

## Documentation Guide

Use the [documentation guide](docs/README.md) to find reference material by
audience and task. Start with [Agent skills and commands](docs/agent-skills-and-commands.md),
[Configuration reference](docs/configuration-reference.md), and
[Sandbox mechanism](docs/sandbox-mechanism.md).

## FAQ

For quick command lookup, start with [CLI Cheat Sheet](#cli-cheat-sheet) and
[Configuration Quick Reference](#configuration-quick-reference).

### Does the agent automatically see my terminal screen?

No. Default model context excludes passive visible screen contents, scrollback,
and alternate-screen contents. The model sees explicit user prompts,
configured instructions, prior conversation context, and explicit action
results.

### Where should API keys go?

Use `mez auth login`. Do not put tokens, API keys, bearer tokens, or other
secret material in config files.

### Can I configure a different shell executable?

Mezzanine uses a usable `$SHELL`, falling back to `/bin/sh`. Shell startup
configuration is intentionally outside the Mezzanine config surface.

### Why do status glyphs shift pane text?

This is usually a terminal-font width mismatch. Adjust `terminal.emoji_width`
in configuration if your terminal renders emoji with a different width.

### How do project instructions work?

Mezzanine discovers project instructions such as `AGENTS.md` and includes them
in the agent context. See the [Configuration reference](docs/configuration-reference.md)
for discovery settings.

### How do project config overlays become trusted?

Project overlays remain pending until the primary client trusts or rejects
them. Review the overlay and project instructions first, then use
`mez sandbox trust list` or `mez sandbox trust add PATH`.

### What happens when a command needs approval?

The runtime routes approval to the primary client according to the active
permission policy. Read-only observers cannot approve, mutate config, or send
pane input.

### Can I use more than one agent at once?

Yes. Agents are pane-scoped, so separate panes can work on separate tasks;
agents can also delegate bounded work to subagents.

### How do I run Mezzanine for automation?

Use `mez serve` to start a foreground service, then target it with `mez -S
<socket>` or `mez -L <name>`. Add `--json` for machine-readable output.

## Contributor Notes

See [AGENTS.md](AGENTS.md) for contributor workflow, validation requirements,
and workspace guidance.
