# Audit and diagnostics

## Purpose

Use security-relevant status and audit information to understand approvals,
sandbox outcomes, authentication changes, and failures without exposing secrets.

## Prerequisites

Have access to the affected session or configured audit-log location.

## Inspect current state

Use `/status` for the active pane's model, policy, writable roots, context, and
token information. Use `/permissions` and `/sandbox` to inspect pane-local
policy and sandbox state. `mez sandbox status --verbose` reports the configured
and effective sandbox projection and bounded diagnostics without changing it.

When an action is blocked, inspect the request before deciding it. A blocked
state is not proof that a command ran; it means execution is waiting for a
primary-client decision. Sandbox setup or launch errors stop the action rather
than silently falling back to host execution.

## Read audit records safely

When enabled, the structured audit log records authentication and permission
changes, approval prompts and decisions, agent-issued shell commands,
configuration changes, subagent work, external connector use, credential-access
attempts, and logout. Records include stable event and session identifiers,
actor, action, policy and approval state, outcome, and redaction metadata.

Audit records redact secrets by default: they must not contain raw credentials,
provider tokens, private keys, or approval secrets. Sandboxed records identify
`bubblewrap` or `seatbelt` and expose only bounded profile version, authority
source, grant counts, effective network mode, and launch-plan digest. Approved
fallback records retain the real origin backend but hash the proof or model
rationale. Records exclude mount or host paths, launcher arguments, generated
SBPL, command content, environment values, artifacts, lifecycle records, probe
output, and raw assessment evidence. Configure the audit path and retention in
the canonical configuration documentation.

Set `audit.hash_chain = true` to cryptographically link consecutive records;
this provides tamper evidence but is not proof against deletion or rollback.
`audit.retention_days` controls age-based pruning. With `audit.required = true`,
a failure to persist a required record denies the auditable work instead of
continuing without its record.

If audit logging is required and unavailable, auditable actions are denied.
Treat a logging failure as an operational problem to repair, not a reason to
disable review controls without a deliberate risk decision.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Operations and troubleshooting](../operations/README.md)
- [Configuration](../configuration/README.md)
- [Normative audit-log contract](../../SPEC.md#26-security-audit-log)

## Next step

Return to [the safety section](README.md) or follow the operations guide when a
session or provider failure needs recovery.
