# Approvals and review

## Purpose

Explain how Mezzanine classifies agent work, requests primary-user decisions,
and applies approval policies without treating approval as confinement.

## Prerequisites

Read the [agent shell guide](../using-mezzanine/agent-shell.md) and know which
project and pane you intend to authorize.

## Review an action

Mezzanine evaluates agent-proposed shell commands, patches, network use,
configuration changes, external integrations, and other effects before they
run. It considers command rules, the active policy, trusted working-directory
state, declared scopes, and whether shell syntax can be safely classified. If
it cannot establish that a command fits the active rules, it asks rather than
assuming it is safe.

Blocked actions remain pending until the primary client decides. Read-only
observers cannot approve, deny, or redirect them. Use the pane-local approval
controls or the session approval view to inspect the exact requested action,
then choose an appropriately narrow decision. A denial returns to the agent so
it can adjust the work; a redirect supplies a new instruction before work
continues.

## Choose a policy deliberately

`ask` prompts for actions that are not already allowed by applicable rules.
`auto-allow` still requires model justification and honors deny rules.
`full-access` suppresses fresh whitelist approval prompts but preserves explicit
denies and any configured sandbox. `host-access` runs local shell actions on
the host outside the sandbox; only the primary user can select it.

Use `/approval` to inspect or change the pane-subtree approval policy and
`/permissions` to inspect rules, presets, and bypass state. Pane overrides
apply to that pane's delegation subtree, not unrelated root panes. Persistent
rule changes deserve the same review as source changes: prefer an exact command
or digest rule over a broad prefix rule.

## Do not confuse approval with isolation

Approval determines whether Mezzanine permits an action. Sandboxing constrains
what an already-permitted local shell process can access. `full-access` does
not disable a configured sandbox, and a Bubblewrap sandbox does not bypass
approval. Approval bypass is a separate, explicit primary-user choice that
disables Mezzanine gating; it is not a promise of safety or host confinement.

## Related pages

- [Sandboxing](sandboxing.md)
- [Project trust and instructions](project-trust-and-instructions.md)
- [Configuration](../configuration/README.md)
- [Normative permissions contract](../../SPEC.md#17-permissions-shell-sandboxing-and-change-review)

## Next step

Read [Sandboxing](sandboxing.md) before relying on filesystem or network
boundaries.
