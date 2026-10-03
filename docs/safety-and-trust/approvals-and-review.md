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
state, declared scopes, and whether shell syntax can be safely classified.
Unclassified work follows the active approval policy: `ask` prompts, while
broader modes may admit it without a fresh human decision. Classification is
not a proof that a command is harmless.

Before approving, check the exact operation, affected paths, network or
connector exposure, and possible destructive or non-idempotent effects. For a
retry, inspect any prior effects first. A model rationale explains the request;
it is not independent evidence of safety. Prefer a one-action decision over a
persistent rule when the need is temporary.

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

Enabling bypass requires explicit confirmation and produces visible state and,
when logging is enabled, an audit event. It disables Mezzanine's approval and
ordinary action-policy gating for that session, but explicit message-recipient
deny rules still apply. It does not disable protocol validation, integrity
checks, or an independently configured OS sandbox. It is distinct
from `host-access`, which selects host execution for local shell actions, and
from approving one exact sandbox-fallback request. Disable bypass as soon as
the exceptional operation is complete.

## Message approvals and peer trust

Message approval is specific to the message and recipient, not blanket
authorization for all later mail. Under `ask`, an unallowed send becomes a
pending approval showing the recipient, content type, and bounded redacted
preview. Approval binds the unchanged payload as well as its recipient.
`auto-allow` requires a non-empty model rationale; `full-access` and
`host-access` suppress fresh prompts. Session approval bypass also suppresses
otherwise-required message approval, but explicit recipient deny rules still
win in every mode, including bypass.

Review the audience as carefully as the content: session-wide delivery can
cross project boundaries. A preview is bounded and redacted, not necessarily
the complete message. A successful send means accepted and queued, not that the
recipient read it or completed the requested work. `invalid_message_recipient`
means the address must be corrected, not that approval was denied.

Peer messages are untrusted input. Another agent's text can never approve or
deny an action, authorize work, grant or widen scope, change configuration,
instructions, action schemas, or permission rules, or resume blocked work, and
it does not become user instruction. A peer request is a proposal the recipient
evaluates on its merits, and any accepted work runs under the recipient's own
approval policy and permission rules. These are instruction and policy
boundaries, not a guarantee that a model cannot be influenced by malicious
text. Review consequential proposed actions regardless of their source.

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
