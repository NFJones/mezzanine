# Mezzanine

Mezzanine is a Unix terminal multiplexer for agentic development. It builds the
`mez` binary and combines persistent sessions, windows, panes, and copy mode
with a pane-scoped AI agent harness.

The normative behavior is defined in [SPEC.md](SPEC.md). This README is a
user-oriented guide to the current repository and daily Mezzanine workflows.

If you are new to Mezzanine, start with [Why Mezzanine?](#why-mezzanine) and
[Quick Start](#quick-start). If you already use Mezzanine,
skip to [Getting Started](#getting-started), [CLI Summary](#cli-summary),
[Documentation Guide](docs/README.md), or
[Configuration Reference](docs/configuration-reference.md).

## Why Mezzanine?

Mezzanine is for terminal-first development where you want multiplexer persistence,
project-local context, and agent assistance without handing an LLM your entire
screen by default.

It is most useful when you want all of the following in one tool:
- **Persistent session management** with windows, panes, detach, reattach, and
  copy mode.
- **Pane-scoped agent context** so one pane can debug a test while another
  stays on logs, a shell, or an editor.
- **Action-mediated execution** where models act through explicit shell,
  patch, approval, MCP, and subagent operations instead of passively reading
  terminal state.
- **Multiplexer-native approval and policy** for shell, network, destructive, and
  configuration-changing actions.
- **Persistent agent workflows** that survive prompt toggles, client detach,
  and session reattach.

If you mainly want a traditional shell multiplexer, use Mezzanine as a multiplexer.
If you mainly want a coding agent, use the agent shell inside the pane where
the work already lives.

## Quick Start

This is the shortest path to first value.

### 1. Build the binary

```sh
cargo build --all-targets --all-features --release
```

The binary is written to `target/release/mez`.

### 2. Create config and log in

```sh
mez config init
mez auth login
```

If you are using an API key instead of browser auth:

```sh
mez auth login --api-key
```

### 3. Start a session

```sh
mez
```

### 4. Open the agent shell in a pane

Press `Alt+]`.

### 5. Try an exact first prompt

From inside a repository, ask the pane agent for a bounded task such as:

> Read this crate, find the most relevant failing or risky area, explain it
> briefly, then propose the smallest safe fix.

That gives new users a real end-to-end workflow: inspect code, reason about a
project-local problem, and act through explicit Mezzanine approvals and shell
transactions.

## Documentation Guide
- [docs/README.md](docs/README.md): documentation map by audience and task.
- [docs/agent-skills-and-commands.md](docs/agent-skills-and-commands.md):
  terminal commands, slash commands, and explicit skills.
- [docs/configuration-reference.md](docs/configuration-reference.md): exact
  configuration fields, defaults, and layer behavior.
- [docs/terminal-compatibility.md](docs/terminal-compatibility.md): terminal
  compatibility coverage summary.
- [docs/examples/config.toml](docs/examples/config.toml): generated baseline
  configuration example.

## Using Mezzanine

- Open a repo in one pane and keep your shell or editor usable.
- Toggle the pane-local agent prompt with `Alt+]`.
- Ask for inspection, debugging, code edits, or repo-local documentation work.
- Review approvals when the active permission policy requires them.
- Detach and reattach later without losing the session layout or pane-local
  agent state.
## Agent capabilities and limits
- The agent can read repo files, run bounded shell commands, apply patches,
  call configured MCP tools, and delegate scoped work to subagents.
- The agent is pane-scoped: it works from the focused pane's working
  directory, conversation state, and live runtime settings.
- The agent does not passively receive your full screen, scrollback, or other
  panes by default. It sees explicit prompts, configured instructions,
  compacted conversation context, and explicit action results.
- The agent is policy-gated. Shell, network, destructive, configuration, and
  some MCP actions may require approval depending on the active runtime mode.
- The agent is not a background daemon with arbitrary host access. It acts
  through explicit Mezzanine operations that can be logged, approved, denied,
  or interrupted.

## Core Features

### Terminal multiplexer

- Persistent sessions that can detach, reattach, list, and terminate.
- Window groups, windows, panes, directional focus, splits, resizing, zoom,
  pane movement, and layout rebalancing.
- Foreground primary clients plus read-only observer attachment with primary
  approval.
- Configurable window and pane frames, status templates, command buttons,
  themes, mouse support, and copy/paste buffers.
- Bounded per-pane history with configurable retention and rotation.
- Xterm-compatible terminal modeling with alternate screen, bracketed paste,
  focus events, mouse reporting, and safe `TERM` defaults.

### Pane agent harness

- Every pane has an associated agent shell, opened by default with `Alt+]`.
- Agent work is pane-scoped. Normal pane input, other panes, and multiplexer keys
  remain usable while an agent prompt is visible.
- Agents interact with the local system through pane shell transactions. They
  do not receive hidden filesystem or process access outside the shell path.
- Model context is action-based. Pane screen contents, scrollback, and
  alternate-screen contents are not passively sent to a model by default.
- Agent turns can inspect files, run bounded shell commands, request approvals,
  call configured MCP tools, communicate with subagents, and update files
  through Mezzanine actions.
- Conversation state, model profile, reasoning preference, log level, and
  pane-local preferences can survive prompt toggles and session reattach.

### Configuration and policy

- Primary user configuration lives under `~/.config/mezzanine/`.
- Supported primary config formats are TOML, YAML, and JSON.
- Project overlays are discovered from project roots and require trust before
  they can expand authority.
- Permission policy covers shell actions, network access, destructive actions,
  command rules, approval routing, and explicit bypass mode.
- Credentials are managed through `mez auth`; secret material is rejected from
  configuration files.
- Audit logging, hooks, snapshots, MCP servers, model profiles, personalities,
  subagents, and local message passing are configurable.

## Requirements

- A Unix-like operating system with pseudoterminals and POSIX-style shells.
- Rust 2024 toolchain when building from source.
- A usable `$SHELL`; otherwise Mezzanine falls back to `/bin/sh` when it is
  executable.
- Provider credentials for model-backed agent work. The generated defaults use
  the built-in OpenAI provider profile.


## Build from Source

```sh
cargo build --all-targets --all-features --release
```

The binary is written to `target/release/mez`.

The repository also provides `just` recipes:

```sh
just build          # debug build
just build-release  # release build
just test           # cargo test --all-targets --all-features
```


## Getting Started

### 1. Create and inspect configuration

```sh
mez config init
mez config path
mez config validate
mez config get
```

Useful config commands:

- `mez config default`: print the built-in default config.
- `mez config get PATH`: show one effective value and its source layer.
- `mez config layers`: show loaded layers and diagnostics.
- `mez config set PATH VALUE`: persist a scalar value.
- `mez config unset PATH`: remove a persisted scalar value.
- `mez config trust list`: inspect project trust records.

### 2. Authenticate a provider

```sh
mez auth login
mez auth status
```

API-key setup is explicit:

```sh
mez auth login --api-key
```

Credentials are stored through the configured credential store and should not be
placed in config files.

### 3. Start or attach to a session

```sh
mez          # default session behavior
mez new      # create a new session
mez list     # list resumable sessions
mez attach   # attach to a resumable session
```

Foreground service mode is available when you want a daemon without immediately
attaching a primary client:

```sh
mez serve
mez attach SESSION_ID
```

Use `-S <socket-path>` to select an explicit control socket or `-L <name>` to
select a named socket under the runtime directory. Add `--json` to CLI commands
when scripting.

### 4. Work in the multiplexer

Default workflow keys follow conventional multiplexer placement:

| Key | Action |
| --- | --- |
| `Ctrl+A :` | Open the Mezzanine command prompt. |
| `Ctrl+A ?` | List key bindings. |
| `Ctrl+A d` | Detach the primary client. |
| `Ctrl+A c` | Create a window. |
| `Ctrl+A %` | Split vertically. |
| `Ctrl+A "` | Split horizontally. |
| `Ctrl+A Up/Down/Left/Right` | Focus a pane by direction. |
| `Ctrl+A n` / `Ctrl+A p` | Next or previous window. |
| `Ctrl+A [` | Enter copy mode. |
| `Ctrl+A ]` | Paste the latest buffer. |
| `Alt+]` | Toggle the focused pane's agent shell. |
| `Alt+Shift+=` | Create a new window group. |
| `Ctrl+Alt+Shift+PageUp/PageDown` | Previous or next group. |

The command prompt accepts Mezzanine commands such as `new-window`,
`split-window`, `select-pane`, `set-theme`, `list-keys`, and `show-options`.
Commands are parsed by Mezzanine, not by the pane shell. Semicolon-separated
command sequences stop at the first failure.

### 5. Use the agent shell

Press `Alt+]` in a pane and type a request. The prompt is pane-local and
non-modal: other panes and multiplexer navigation remain available.

Useful slash commands include:

| Command | Purpose |
| --- | --- |
| `/help` | Show agent shell help. |
| `/status` | Show the current pane agent session. |
| `/model` | Inspect or change model selection through the live runtime. |
| `/approval` | Inspect or change approval mode through the live runtime. |
| `/permissions` | Inspect or change permission policy through the live runtime. |
| `/list-skills` | Show the skills available to the active pane. |
| `/list-mcp` | List configured MCP tools. |
| `/log-level` | Show or set `normal`, `verbose`, `debug`, or `trace`. |
| `/stop` | Interrupt the active turn. |
| `/new` | Start a fresh conversation for the pane. |
| `/resume` | Resume a saved conversation through the live runtime. |
| `/compact` | Compact conversation context through the live runtime. |
| `/exit` | Hide the agent shell through the live runtime. |

Agent work is approval-gated by the active policy. Normal logging shows prompts,
assistant text, concise progress, approvals, errors, command summaries, and
final responses. Use higher log levels only when debugging.
Mezzanine has three operator-facing command surfaces:
- terminal commands entered through the Mezzanine command prompt,
- pane-local slash commands entered in the agent shell, and
- explicit skills invoked with `$<skill-name> [additional context]`.

See [docs/agent-skills-and-commands.md](docs/agent-skills-and-commands.md) for
the command-surface breakdown, explicit skill syntax, and built-in skill usage.

### 6. Project workflow

- Put project-specific agent instructions in `AGENTS.md`.
- Put project config overlays under `.mezzanine/config.toml` when needed.
- Project overlays are trusted per project root. Until trusted, behavior that
  depends on the overlay is blocked or skipped with diagnostics.
- Inspect trust state with `mez config trust list`; trust, reject, or revoke
  project roots through `mez config trust ...`.

## Example workflows

### Debug a failing test in the current repository

1. Start or attach to a Mezzanine session in the repo.
2. Press `Alt+]` in the pane that already has the repo working directory.
3. Ask:

   > Run the smallest relevant test target, explain the failure, fix it with
   > the smallest coherent patch, and rerun the check.

This workflow highlights the main Mezzanine model: the agent stays scoped to
the current pane and works through explicit shell and patch actions instead of
implicit terminal scraping.
### Delegate a bounded investigation to subagents
1. Open the agent shell in the pane where the repo already lives.
2. Ask for a split task such as:
   > Inspect this regression and delegate targeted read-only investigation to
   > subagents for the scheduler and runtime paths. Summarize the findings and
   > recommend the smallest safe fix.
3. Review any additional panes or windows created for delegated work and the
   final parent-agent summary.
This workflow shows that Mezzanine can coordinate multiple pane-local agents
without collapsing all work into one conversation. Subagent limits, placement,
and wait behavior are configurable.
### See approval and policy boundaries in practice
1. Start a session in a repository and open the agent shell with `Alt+]`.
2. Ask for a task that needs execution, for example:
   > Run the smallest relevant test command, explain the result, and propose a
   > patch if needed.
3. When approval is required, review the requested action in the primary
   client before allowing or denying it.
This makes the operating model concrete: the agent can inspect and act, but it
does so through explicit policy-controlled operations rather than hidden host
access.

### Review a project overlay before trusting it

1. Open the project in a pane.
2. Inspect pending trust state:

   ```sh
   mez config trust list
   ```

3. Review `.mezzanine/config.toml` and `AGENTS.md`.
4. Trust the project root only after you understand the additional authority it
   requests.

This matters because project overlays can expand behavior, but only after an
explicit trust decision by the primary client.
## What persists across the session
- Session layout, windows, panes, and pane history persist according to the
  active session and history settings.
- Pane-local agent conversation state can survive prompt hide/show, detach, and
  reattach flows.
- Live agent settings such as the selected model, approval mode, and log level
  can remain associated with the pane agent session.
- Project trust decisions and persisted configuration changes remain in the
  relevant config or trust store, not just the live client process.
- In-flight work still follows runtime policy: persistence keeps context and
  resumable state, but does not bypass approval or trust checks.

## CLI Summary

```text
mez [--json] <command> [options]
```

Common commands:

| Command | Purpose |
| --- | --- |
| `new`, `new-session` | Start a background session daemon and attach when interactive. |
| `serve`, `daemon` | Start a foreground control daemon for a new session. |
| `list`, `list-sessions` | List resumable sessions. |
| `attach`, `attach-session` | Attach as primary or request observer access. |
| `detach`, `detach-client` | Detach the current or specified client. |
| `kill-session --force` | Terminate a live session. |
| `snapshot ...` | Create, list, inspect, delete, resume, and plan snapshots. |
| `config ...` | Initialize, inspect, validate, mutate, and trust configuration. |
| `auth ...` | Login, show status, or logout. |
| `mcp ...` | List, add, remove, enable, disable, and inspect MCP servers. |
| `memory ...` | List, add, inspect, edit, delete, and export persistent memory. |
| `help` | Show CLI help. |
| `version`, `--version` | Show version information. |

## Configuration Reference
Use the dedicated configuration reference for generated defaults, supported fields, and layer behavior:
- [Configuration reference](docs/configuration-reference.md)
- [Example config](docs/examples/config.toml)
- [SPEC.md Section 8](SPEC.md#8-configuration)

If you are new to Mezzanine, start with `mez config init`, `mez config get`,
and `mez config validate`, then use the reference when you need exact field
names and defaults.

## Development Checks

Use the repository `justfile`:

```sh
just check
just fmt
just clippy
just test
```

`just fmt`, `just clippy`, and `just test` are the expected pre-handoff checks
for repository changes.

## FAQ

### Does the agent automatically see my terminal screen?

No. Default model context excludes passive visible screen contents, scrollback,
and alternate-screen contents. The model sees explicit user prompts,
configured instructions, compacted prior context, and explicit action results.

### How do I enter and leave the agent shell?

Press `Alt+]` in the focused pane. Use `/exit`, `Ctrl+D` on an empty prompt, or
Escape to hide it. If a turn is running, hiding the prompt requests `/stop`
before normal pane input resumes.

### How do I change the active theme?

Use the terminal command `set-theme NAME`, or persist a lower-level config value
with:

```sh
mez config set theme.active nord
```

Run `list-themes` from the command prompt to inspect available themes.

### How do I change models?

Edit `providers`, `model_profiles`, and `agents.default_model_profile`, or use
the live `/model` agent command when the runtime is active. The generated
defaults define OpenAI profiles for `gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini`, and
`gpt-5.3-codex`.

### Where should API keys go?

Use `mez auth login`. Do not put tokens, API keys, bearer tokens, or other
secret material in config files.

### Can I configure a different shell executable?

No. Mezzanine resolves the shell from `$SHELL` when it is absolute and
executable, otherwise from `/bin/sh`. Config may adjust shell mode and
environment, but not the executable path.

### How do project instructions work?

By default Mezzanine discovers `AGENTS.md` from the project context and includes
it in provider requests. The discovery filenames are configurable under
`instructions.project_filenames`.

### How do project config overlays become trusted?

Project overlays are discovered from the project root and remain pending until
the primary client trusts or rejects them. Use `mez config trust list` to see
records and `mez config trust trust PATH` to trust a root.

### What happens when a command needs approval?

The runtime routes approval to the primary client according to the active
permission policy. Read-only observers cannot approve, mutate config, or send
pane input.
### Can I use more than one agent at once?
Yes. Agents are pane-scoped, so you can open agent shells in different panes
for separate tasks. Mezzanine can also spawn subagents for delegated work,
subject to the configured depth, placement, and concurrency limits.

### How do I run Mezzanine for automation?

Use `mez serve` to start a foreground service, then target it with `mez -S
<socket>` or `mez -L <name>`. Add `--json` for machine-readable output.

### What is the simplest first real task for a new user?
Open a repository in a pane, press `Alt+]`, and ask the agent to inspect one
bounded problem such as a failing test, a suspicious module, or a documentation
gap. The best first experience is a small request that requires reading local
files and possibly running one or two focused commands.


### How do I inspect the exact generated default config?

Run:

```sh
mez config default
```
