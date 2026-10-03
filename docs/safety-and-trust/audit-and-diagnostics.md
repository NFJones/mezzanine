# Audit and diagnostics

## Purpose

Use security status and audit records to investigate approvals, confinement,
authentication, and failures without assuming they are complete forensic proof.

## Prerequisites

Have access to the affected session and the configured audit destination.
Treat diagnostics as sensitive until reviewed.

## Inspect current state

Use `/status` for the active pane's model, policy, roots, context, and token
information. Use `/permissions` and `/sandbox` for pane-local policy and sandbox
state. `mez sandbox status --verbose` is a read-only report of configured and
effective sandbox state with bounded diagnostics.

A blocked approval is not evidence that a command ran. Conversely, a timeout
or missing completion record is not evidence that it did not run. When effects
are uncertain, inspect the resulting state before retrying. Sandbox setup errors
do not silently authorize host execution; see [Sandboxing](sandboxing.md).

## Configure and verify logging

Configure `audit.enabled`, `audit.path`, `audit.retention_days`,
`audit.hash_chain`, and `audit.required` in primary-user configuration; consult
the [configuration guide](../configuration/README.md) for their defaults and
live-change behavior. Keep the destination in a private, operator-controlled
directory with sufficient space. Do not point it at an untrusted repository,
shared writable directory, or symlink.

When enabled, structured records cover security-relevant events such as
authentication, permissions, approvals, shell execution, configuration,
subagents, and connectors. Fields include event/session identity, actor, action,
policy, approval state, outcome, and redaction metadata. Correlate those fields
with the affected session and time; an approval decision alone does not prove
successful execution or durable recording of its outcome.

### Required logging has a current durability limitation

SPEC requires auditable actions to be denied while required logging is
unavailable. `audit.required = true` rejects logging that is disabled, and the
synchronous writer reports write failures. **The running daemon also queues
audit writes asynchronously: an accepted action or policy change can precede
durable persistence, even with required logging.** Persistence failure is
reported later and cannot undo already-applied effects. Do not rely on this
setting as a universal write-before-execution guarantee or proof that no work
occurred during a storage outage.

For deployments requiring that stronger guarantee, treat the gap as a release
or compliance blocker. Verify the destination after startup and policy changes,
monitor daemon/service-manager persistence errors and disk capacity, and stop
new agent work on a logging failure until the destination is repaired. Preserve
existing records and inspect potentially applied effects before retrying. Do not
disable required logging merely to hide the failure.

## Understand redaction and integrity limits

Known credential values are omitted by dedicated event producers, and generic
record sanitization redacts recognized secret-like strings. MCP argument
sanitization also checks sensitive keys recursively. These are not detectors for
every arbitrary secret: an unfamiliar credential, task detail, or identifying
metadata can escape pattern matching. Review records before sharing them and
never deliberately put secrets into commands, configuration values, or reports.
Private permissions do not protect against other processes running as the same
account.

Shell sandbox records describe the actual execution boundary, enforcement,
network mode, and reason. A sandbox backend name requires a compiled plan;
configured intent alone is insufficient. Sandboxed records carry bounded,
path-free profile/grant summaries and a plan digest, not launcher arguments,
environment values, or raw lifecycle evidence. Approved unsandboxed fallback
is recorded as `policy-only`, with its original backend, approving client,
partial-effect warning, and retry outcome. These records are diagnostic evidence,
not an independent attestation of the host.

`audit.hash_chain = true` links consecutive records within a writer's chain.
The hashes are unkeyed; a party able to rewrite the log can recompute them.
Restart, retention, deletion, rollback, and multiple writers also limit what a
local chain proves. Use protected external collection and independently retained
checkpoints if the deployment needs stronger tamper evidence. Age-based retention
prunes local evidence; establish any required archival policy before pruning.

## Collect a safe incident report

1. Preserve the time, session/pane identifiers, exact symptom, and relevant
   bounded action result. Note whether execution was blocked, failed, or unknown.
2. Capture effective policy and sandbox status rather than only configuration.
3. Preserve private daemon/service-manager diagnostics and audit records without
   editing the originals. Check that expected records actually reached storage.
4. Make a reviewed, redacted copy for sharing. Trace exports, transcripts,
   terminal output, snapshots, and backups have different content from the audit
   log and can include task data; do not assume audit redaction applies to them.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Troubleshooting](../operations/troubleshooting.md)
- [Cache status and diagnostics](../operations/cache-status-and-diagnostics.md)
- [Normative audit-log contract](../../SPEC.md#26-security-audit-log)

## Next step

Use [Troubleshooting](../operations/troubleshooting.md) to repair the symptom,
retaining the evidence needed to distinguish a retry from an uncertain replay.
