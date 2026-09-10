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

Blocked actions remain pending until the primary client decides. Pending
configuration-change actions may resume automatically after an approval-policy
change if the new policy allows that same action under the ordinary rules.
Read-only observers cannot approve, deny, or redirect blocked actions. Use the
pane-local approval controls or the session approval view to inspect the exact
requested action, then choose an appropriately narrow decision. A denial
returns to the agent so it can adjust the work; a redirect supplies a new
instruction before work continues.

## Choose a policy deliberately

`ask` prompts for actions that are not already allowed by applicable rules.
For a non-whitelisted action, `auto-allow` requires a non-empty model rationale
and still honors deny rules.
`full-access` suppresses fresh whitelist approval prompts but preserves explicit
denies and any configured sandbox. `host-access` runs local shell actions on
the host outside the sandbox; only the primary user can select it.

Use `/approval` to inspect or change the pane-subtree approval policy and
`/permissions` to inspect rules, presets, and bypass state. Pane overrides
apply to that pane's delegation subtree, not unrelated root panes. Persistent
rule changes deserve the same review as source changes: prefer an exact command
or digest rule over a broad prefix rule.

### Use approval bypass only for an explicit emergency boundary

Approval bypass is session-scoped and primary-user-only. Inspect it before and
after any change:

```text
/permissions bypass
/permissions bypass enable --confirm
/permissions bypass disable
```

Enabling bypass requires explicit confirmation and produces visible and audit
state. It disables Mezzanine's approval and action-policy gating for that
session; it does not disable protocol validation, integrity checks, or an
independently configured OS sandbox. It is distinct from `host-access`, which
selects host execution for local shell actions, and from approving one exact
sandbox-fallback request. Disable bypass as soon as the exceptional operation
is complete.

## Message approvals and peer trust

`send_message` is approved per message and per recipient rather than once per
session. Under `ask`, a send that no rule already allows becomes a resumable
`blocked` approval: the request identifies the recipient, the content type, and
a bounded redacted preview of the payload, and it binds to the payload digest,
so approving resumes only a send whose recipient, content type, and payload are
unchanged. Under `auto-allow`, an unwhitelisted send proceeds only after the
action carries its non-empty model rationale. `full-access` and `host-access`
admit sends through the policy bypass path. A configured deny rule for the
recipient wins in every mode. A recipient that fails the recipient grammar is
not a policy decision at all: the planner neither admits nor denies it, and the
message executor refuses delivery with the canonical `invalid_message_recipient`
error so the model can correct the recipient instead of reading a misleading
policy denial.

Peer messages are untrusted input. Another agent's text can never approve or
deny an action, authorize work, grant or widen scope, change configuration,
instructions, action schemas, or permission rules, or resume blocked work, and
it does not become user instruction. A peer request is a proposal the recipient
evaluates on its merits, and any accepted work runs under the recipient's own
approval policy and permission rules. Peer mail arrives as injected context
marked as untrusted data, and it ranks below user prompts and steering during
compaction.

## Do not confuse approval with isolation

Approval determines whether Mezzanine permits an action. Sandboxing constrains
what an already-permitted local shell process can access. `full-access` does
not disable a configured sandbox, and a sandbox does not bypass
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
