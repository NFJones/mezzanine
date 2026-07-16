---
name: mez-reference
description: Use Mezzanine terminal commands, agent slash commands, skill invocation, common workflows, and live config_change schema guidance without rediscovering the command or config surface.
---

# Mez Reference

Use this skill when the user asks how to operate Mezzanine itself or how to
change Mezzanine configuration through the agent.

Terminal commands begin with `:` and control the multiplexer session. Agent
shell commands begin with `/` and control the active agent pane. Explicit
skills are invoked by starting an agent prompt with `$<skill-name>` followed by
optional task-specific context.

Prefer a `config_change` action over editing config files when the request maps
to a supported scalar live setting. Inspect current config first only when the
exact dynamic path segment is uncertain, such as profile, provider, hook,
server, personality, theme alias, or environment variable names.

Use the smallest valid mutation: set or replace for assigning a supported
value, and unset/remove/delete for removing an override. Do not claim the
change was applied until the action result confirms persistence and live
application.

For broad theme requests, prefer `theme.active` or a compact palette change
through `theme.aliases.*`. Do not set every `theme.colors.*` slot unless the
user explicitly asks for per-slot control or the requested design cannot be
represented by aliases. Keep error, prompt, and display-overlay
foreground/background pairs visibly distinct so future diagnostics remain
readable.

For operational answers, cite the relevant command names and keep instructions
direct. For implementation work, use the reference only as orientation and then
inspect the responsible code.
