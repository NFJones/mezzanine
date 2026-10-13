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

Use a dedicated directory: logging creates missing storage and applies `0700`
to the immediate parent and `0600` to the log file. Choosing an existing shared
directory can therefore change its permissions. Relative `audit.path` values
resolve beneath the Mezzanine configuration root.

**Current path-safety limit:** audit file appends do not reject a symlink at the
destination or use a no-follow file open. The asynchronous writer checks the
immediate parent for a symlink, but this is not descriptor-based protection of
the entire path. Protect every parent directory from untrusted replacement;
owner-only permissions applied after opening are not proof of safe path
resolution. Do not assume the stronger remote credential-file handling also
applies to audit logs.

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

## External accounting recovery privacy and limits

Same-live external source recovery keeps bounded native lifetime anchors and
inert current/retired observer selectors in the existing control service's memory.
It does not persist credentials, active capabilities, private callbacks,
transcripts or lifecycle projections. The private accounting database continues
to store opaque source mappings, request provenance and replay tombstones; those
rows cannot activate a source or prove vendor identity/invoice completeness.
An exact current live lifecycle binding can validate its public receipt and
retry/rotate even without an accounting store. This is not namespace recovery:
the original owner and unavailable-reporting flag cannot be upgraded, and no
new durable recovery acknowledgment is created. An already recovered handle's
idempotent retry preserves its original receipt without resetting the projection.
Missing-registration recovery still
requires the original durable mapping and retained native service authority.
If a retained ancestor exits, cleanup releases that unusable proof while keeping
the surviving creator's retired source/instance fence. This is not new-source
admission, process-death inference or accounting deletion. Diagnostic refresh
cannot resurrect it; already-admitted expense still settles under its original
owner. Descriptor budget is released when all legitimate shared references end.

Recovery is limited to disposable registry/actor-task loss while the same service,
actual creator and pane root remain alive. Complete service/OS-daemon replacement,
retirement, replaced sources or missing native evidence fail closed.
Snapshot/session replacement retires all retained source authority; it is not
the disposable registration loss which the bounded protocol can recover.
Losing the recovered registry again before its new receipt is known is a named unavailable
retry, not permission to guess the newest owner or recharge old usage.
An initial mapping whose native admission never published also remains inert;
bare initial packets cannot recreate a missing service authority from it. Preserve
the original immutable report and request boundary; retry reporting, never paid
provider work. Reports the daemon never received remain a visible coverage gap.
Native Linux fixtures do not by themselves qualify installed vendors or macOS.

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
