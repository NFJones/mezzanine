# Workflows

## Purpose

Apply Mezzanine's session and agent features to routine, reviewable work.

## Prerequisites

Know [Sessions and panes](sessions-and-panes.md) and the [Agent shell](agent-shell.md).

## Investigate and change a repository

Start with a narrow request that names the goal and desired validation. For
example:

> Find the owner of this failing test, make the smallest safe correction, run
> the focused check, and summarize the result.

Review requested actions before approval. Keep independent investigations in
separate panes when useful; each pane owns its own agent conversation and
working context. Ask for a concise plan first when the change is broad or a
decision needs review.

## Use reusable prompts and coordination

Begin a prompt with `$<skill-name>` to invoke an available skill or
`#<macro-name>` for an ordered macro. Use `@<mcp-server-name>` only when the
task needs a configured MCP integration; injected tool details apply to that
turn rather than becoming permanent context.

Use subagents for bounded, separable work and keep the parent responsible for
integration. See the agent guide for routing, messaging, continuity, and
provider behavior.

## Recover and continue

Use `/status` to inspect the current pane's model, policy, context, and token
state. Use `/stop` to interrupt work, `/new` to start a fresh conversation, and
`/resume` to return to a saved one. Detach and reattach sessions when the client
must leave; use operations guidance for service, diagnostic, or recovery issues.

## Related pages

- [Agent and integrations](../agent/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)

## Next step

Read [Agent and integrations](../agent/README.md) for commands, skills,
subagents, providers, and MCP.
