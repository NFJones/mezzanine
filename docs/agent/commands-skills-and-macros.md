# Commands, skills, and macros

## Purpose

Choose between multiplexer commands, agent slash commands, reusable skills,
and ordered macros without confusing their scope or safety boundaries.

## Prerequisites

Open the [agent shell](../using-mezzanine/agent-shell.md) in a pane.

## Choose the control surface

| Surface | Use it for |
| --- | --- |
| Multiplexer command prompt (`Ctrl+A :` by default) | Session, window, pane, copy, and presentation controls; parsed by Mez, not the pane shell. |
| Pane shell | Ordinary shell commands, including the `mez` CLI. |
| Agent prompt | Natural-language tasks and agent slash commands. |
| `$<skill-name>` at the start of an agent prompt | Load a reusable workflow for this task. |
| `#<macro-name>` at the start of an agent prompt | Run an ordered, model-orchestrated sequence of prompts. |
| `@<server-id>` in an agent prompt | Reference a configured MCP integration for metadata retrieval. |

Use `/help` and the multiplexer command prompt's `help` for the effective live
catalog. Bindings and capabilities can vary with configuration. CLI commands
have `--help`; do not type a `mez ...` command as a slash command.

## Use operational slash commands

Slash commands control the pane or invoke a defined workflow instead of being
ordinary prose requests. Some, such as `/remember`, `/compact`, and `/loop`,
can involve model work. Commands that need store or filesystem work may show
`command running` and stop accepting prompt input until their result appears;
other panes and global multiplexer controls remain available.

| Goal | Commands |
| --- | --- |
| Inspect or change authority and execution mode | `/status`, `/permissions`, `/approval`, `/approve`, `/show-approvals`, `/sandbox`, `/shell-mode` |
| Control the current task | `/plan`, `/directive`, `/objective`, `/loop`, `/stop` |
| Manage conversations | `/new`, `/clear`, `/fork`, `/resume`, `/name-session` |
| Inspect or preserve context | `/compact`, `/context-doc`, `/show-context`, `/copy`, `/copy-context`, `/copy-patches`, `/copy-trace-log`, `/list-modified-files` |
| Select model behavior | `/model`, `/routing`, `/latency`, `/thinking`, `/personality`, `/list-personalities` |
| Work with local stores | `/memory`, `/remember`, `/show-memories`, `/issue`, `/show-issues` |

### Check before changing state

```text
/status
/permissions
/approval
/shell-mode status
/routing status
```

Bare `/routing` **toggles** routing. Use `/routing status` for inspection.
`/plan on` enables plan-only mode and removes write scopes for later turns;
use `/plan off` before asking for edits. `/approve` decides a pending action
in the current pane; `/show-approvals` also finds requests in other panes.
Review the action's scope, not just its description.

`/sandbox` reports or changes pane-local sandbox state. Advanced setup,
profiles, and managed-home cache operations remain CLI-only under `mez sandbox`.
`/shell-mode native` or `/shell-mode pane` changes the pane's shell transport;
add `--global` to persist the fallback for panes without an override. Transport
does not change approval or [sandbox authority](../safety-and-trust/sandboxing.md).

### Keep or inspect task history

Use `/new` for unrelated work, `/fork` for an independent branch, `/resume`
for saved work, and `/compact` to summarize older completed work. `/clear`
also starts fresh and clears the view; it does not undo file changes. See
[Context and continuity](context-and-continuity.md) for retention, archives,
context documents, and the difference between history and persistent memory.

`/objective <text>` sets a published objective for the current conversation;
it takes precedence over generated objectives until `/objective --clear`.
`/directive <text>` supplies pane-session guidance for future turns. Neither
command grants permissions.

`/show-context` browses conversation entries, with `e` to edit and `d` to
delete selected content. `/show-issues` uses `e` for body and `E` for notes;
`/show-memories` uses `e` for content. `/memory edit <uuid>` and `/issue edit
<id> body|notes` open only the selected prose in the external editor. Metadata
uses typed commands. `/issue` manages Mez's project issue store, not an external
tracker. On a concurrent-edit conflict, Mez retains the private draft for
`/editor-recovery` rather than overwriting the newer record.

External editors run on the **Mez server**, not the attached client's machine
or through the pane shell. This matters when attached remotely. `/init` creates
an `AGENTS.md` scaffold only when absent; it does not overwrite existing guidance.

### Diagnose or stop work

`/auth-status`, `/refresh-provider-info`, `/debug-config`, `/reset-status`, and
`/log-level` inspect authentication, refresh provider information, inspect
configuration, reset token counters, and control verbosity. They do not grant
provider entitlement. See [Providers and models](providers-and-models.md).

During a running turn, `/copy-context` exports its assembled provider request;
when idle, it exports an unsent next-request preview. `/copy-patches` exports
retained patch payloads and outcomes. `/copy-trace-log` exports retained
diagnostics, while `/list-modified-files` reports files changed by the current
conversation. Review diagnostic exports before sharing private task data.

Use `/stop` for unwanted active work and `/exit` to hide the agent shell after
work stops. Use `/loop` only for bounded repeated work; see its
[stopping rules](subagents-and-messaging.md#use-routed-loops-sparingly) before
relying on a loop's completion claim.

## Invoke a skill or macro explicitly

### Skills: load a workflow

Use `/list-skills` to inspect the effective catalog, then start a prompt with
`$<skill-name>` and task-specific context. For example, the built-in creation
workflow can help install a skill:

```text
$create-skill Create a user skill for read-only release-checklist reviews.
```

User skills live under `~/.config/mezzanine/skills/<name>/SKILL.md`; trusted
project skills live under `.mezzanine/skills/<name>/SKILL.md`. Project skills
are discovered only after project trust. A trusted project entry overrides a
user entry of the same name, and both override the built-in entry. Inspect the
catalog's source before invoking an unfamiliar name.

A skill file needs YAML front matter with `name` and `description`, followed
by Markdown instructions. The directory name must match `name`, using only
lowercase ASCII letters, digits, and hyphens. Auxiliary scripts or references
are not automatically executed or loaded just because you invoke the skill.
Model-selected skill discovery and loading are disabled and are not available
through `agents.enabled_actions`. Use `/list-skills` and invoke the chosen
workflow explicitly with `$<skill-name>`.

`/sync-builtin-skills` restores managed built-in copies in the user configuration
root. It preserves valid user overrides that omit the managed-version marker
and does not change project skills. Review the reported replacements if you
have edited a managed copy. Skills remain workflow guidance, not permission
to bypass approvals or action rules.

### Macros: run ordered steps

Use `/list-macros`, then start a prompt with `#<macro-name>` and any invocation
context. A macro token later in ordinary prose does not start a macro. User
macros live under `~/.config/mezzanine/macros/<name>/MACRO.md`; trusted project
macros live under `.mezzanine/macros/<name>/MACRO.md`, take precedence over a
same-named user macro, and require project trust.

Macro files use the same name and description front matter and name grammar
as skills. Their body needs a `## Steps` section containing an ordered list of
non-empty prompts. Use `$create-macro` to ask the built-in workflow to create
or revise one.

One persistent subagent runs the entire sequence. After each step, the main
model judges the result and may continue, adapt the next prompt within the
macro's purpose, retry the current step, or stop on failure. A macro is not
an unconditional shell script: steps use normal prompt parsing, permissions,
approvals, and delegation limits, and success requires all required steps in
order. Review its definition and side effects before invoking it.

For external tools, use a configured `@<server-id>` reference and follow
[MCP integration](mcp-integration.md). The mention permits metadata retrieval,
not automatic tool execution or approval.

## Related pages

- [Subagents and messaging](subagents-and-messaging.md)
- [MCP integration](mcp-integration.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Safety, trust, and security](../safety-and-trust/README.md)

## Next step

Read [Subagents and messaging](subagents-and-messaging.md) before delegating
work or using a routed loop.
