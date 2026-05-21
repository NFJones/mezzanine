---
name: mez-config
description: Use config_change correctly by choosing supported scalar Mezzanine setting paths, operations, and value shapes from the live annotated schema.
---

# Mez Config

Use this skill when the user asks to change Mezzanine configuration through the agent.

Prefer a `config_change` action over editing config files when the request maps to a supported scalar live setting. Inspect current config first only when the exact dynamic path segment is uncertain, such as profile, provider, hook, server, personality, theme alias, or environment variable names.

Use the smallest valid mutation: set or replace for assigning a supported value, and unset/remove/delete for removing an override. Do not claim the change was applied until the action result confirms persistence and live application.

For broad theme requests, prefer `theme.active` or a compact palette change through `theme.aliases.*`. Do not set every `theme.colors.*` slot unless the user explicitly asks for per-slot control or the requested design cannot be represented by aliases. Keep error, prompt, and display-overlay foreground/background pairs visibly distinct so future diagnostics remain readable.
