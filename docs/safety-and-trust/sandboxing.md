# Sandboxing

## Purpose

Understand what confines an approved local shell action, what remains outside
that boundary, and how to respond when confinement cannot be established.

## Prerequisites

Read [Approvals and review](approvals-and-review.md). Approval permits an
action; it does not isolate it.

## Check the actual boundary

Inspect the affected pane with `/permissions` and `/sandbox`. For a read-only
CLI report, use `mez sandbox status --verbose`; `mez sandbox plan` previews
setup without proving that an action ran under confinement.

| Backend | Protection and limits |
| --- | --- |
| `policy-only` | Approval classification, not OS filesystem or shell-network confinement. |
| `bubblewrap` | Linux namespace confinement with authorized read-only/read-write mounts. Requires the fixed `/usr/bin/bwrap` executable and a successful runtime capability probe. |
| `seatbelt` | macOS operation-level access controls through `/usr/bin/sandbox-exec`. Host namespaces remain visible; this is not Bubblewrap-equivalent namespace isolation. Apple's deprecated interface may be unavailable on future macOS releases. |

New Linux and macOS configurations select `full-access` with their platform
sandbox when its fixed executable is available, and `auto-allow` with
`policy-only` otherwise. Existing configurations are preserved by migration.
Neither a default nor executable presence proves runtime enforcement. Confirm
the effective state before relying on it, particularly after installation or
an OS upgrade.

Status distinguishes configured intent from effective enforcement. `unavailable`
means the backend executable is missing; `not-probed` with `unknown` networking
means no matching runtime proof and compiled launch plan are available.
`policy-only` and host access have `none` enforcement and `unenforced`
networking. An `isolated` or `connected` network claim requires both the plan
and its capability proof. A CLI preview is not an attestation of every action
or remote process; inspect the affected action's result as well.

### Native shell mode is not process-free patching

Pane mode sends agent work through the pane shell. Native shell mode runs a
fresh process outside the pane PTY and can work while a full-screen application
occupies the pane. Both modes use the configured local sandbox when applicable.

**Native `apply_patch` still uses the shell-backed read/write path and reports
`spawned_shell`, not `native_runtime`.** The process-free filesystem adapter is
planned, not integrated. Existing filesystem primitives and tests do not supply
that runtime guarantee. Do not rely on future descriptor-based publication,
commit fencing, or cancellation protections for today's patches. See the
[migration contract](../../SPEC.md#process-free-semantic-adapter-contract-and-migration)
for the planned behavior.

## Grant filesystem access narrowly

`permissions.read_scopes` and `permissions.write_scopes` define user filesystem
authority; write scopes also imply reads. Bubblewrap exposes authorized paths
as mounts, while Seatbelt controls operations on canonical host paths. Fixed
runtime support paths and private temporary storage are also available, and
effective authority can include Mezzanine's user skills and macros roots.
Read the effective report rather than assuming the project is the only visible
directory.

With both scope arrays empty, a trusted project supplies its canonical root as
default read/write authority. The deepest stored trust decision governs this
default: a nested rejection or revocation withholds it, while a nested repository
without its own decision retains recursive parent trust. Explicit configured
scopes are a separate grant and are not removed by rejecting an overlay. See
[Project trust and instructions](project-trust-and-instructions.md).

Unavailable configured paths are excluded with a warning, not replaced by a
broader scope. The multi-user `/home` root cannot be an authority scope. Avoid
granting a whole home directory or credential-bearing paths merely to make a
tool work. Under `policy-only`, scopes and trust checks are not OS confinement:
an admitted shell process is not physically restricted to those paths.

Trusted-project sandbox runs use private managed homes when a private
configuration root is available. Neither backend copies the real home,
credentials, or global Git configuration into that home. This does **not** mean
credentials are inaccessible: explicitly authorized paths, forwarded environment
values, and network access can expose them. Plan managed-home cleanup and quotas
as deployment policy.

Each sandboxed action receives a private writable temporary directory. Seatbelt
also authorizes the resolved macOS per-user temporary root for tools such as
BSD `mktemp`; temporary access is therefore broader than only the private
`TMPDIR`. Forwarding an XDG path does not authorize filesystem access to it.

On macOS, Seatbelt's code-owned runtime profile additionally permits read-only
access to `/Library/Developer/CommandLineTools` and `/opt/homebrew` when each
is an existing real directory. These loader and SDK reads do not enter
configured `permissions.read_scopes`, do not grant write access, and do not
replace the trusted-project fallback. Missing roots add no profile rule.

### Inspect and clean managed Bubblewrap homes

Use the dedicated maintenance commands rather than deleting managed homes by
hand. These commands manage Bubblewrap homes, not provider prompt caches or
Seatbelt temporary storage:

```sh
mez sandbox cache status PATH
mez sandbox cache clear PATH --dry-run
mez sandbox cache clear PATH --yes
mez sandbox cache prune --dry-run
```

Replace `PATH` with the project directory; omit it to use the current directory.
`clear` previews unless `--yes` confirms deletion. `prune` previews all inactive
homes; add `--yes` only after reviewing that broader candidate set. `--dry-run`
remains non-mutating even with `--yes`. Maintenance is limited to the private
managed-home root, rejects symlinks and unsupported entries, and skips homes
locked by active workloads. There is no automatic periodic or age-based pruning
or persisted quota policy. Trust revocation does attempt best-effort removal of
the affected project's managed home; verify the outcome rather than assuming
cleanup succeeded. Cleanup removes managed-home files and caches, not project files, and
does not revoke project trust or change explicit scopes.

## Review environment and integration exposure

Native launches start from a cleared environment, using a small set of runtime
requirements and optional values selected by `permissions.env_whitelist` from
the Mez server's startup environment. Pane-root metadata helps infer the shell,
working directory, and process identity; its environment is not forwarded
wholesale. Deliberately forwarded credentials can still reach native work.
Sandbox payloads use their managed environment and configured whitelist;
semantic patches do not forward optional environment values. Do not treat
environment composition as credential removal or assume an interactive shell
export changes a native action's environment. Configure only required forwarding
and check the mode and effective policy when a tool loses `PATH`, proxy,
toolchain, or agent-socket access.

These boundaries apply to permitted local shell work, not to the entire
Mezzanine daemon or every integration. Web, fetch, and MCP actions have separate
capability and approval gates. An external MCP server can have its own filesystem,
process, credential, and network access outside shell confinement. Review its
configuration and declared effects independently; a sandboxed shell does not
make a connector safe.

## Control shell networking

`permissions.network_policy` controls shell networking:

- With Bubblewrap, `deny` isolates the network namespace.
- With Seatbelt, `deny` rejects TCP, UDP, and Unix-domain socket operations in
  the visible host namespace.
- `allow` permits networking; Seatbelt includes the system services needed for
  ordinary host-client networking. With `prompt`, an admitted action classified
  as requiring networking can receive a connected sandbox plan. Admission
  follows the active approval policy and applicable rules: `full-access` can
  admit it without a fresh human prompt, and `auto-allow` can use the model's
  rationale. The policy name is not a guarantee of a per-action human decision.

Neither backend filters destinations. Allowing networking can permit data
exfiltration from readable paths. `policy-only` does not enforce shell-network
isolation even when policy classification rejects known network actions.
Provider, web, fetch, and MCP traffic are not governed by a child shell's network
namespace or Seatbelt profile.

## Recover without silently weakening protection

Probe, profile, authority, setup, and launch failures stop the action rather than
silently switching to the host. First inspect the diagnostic and effective
status; repair the backend or choose a sandbox-preserving alternative.

Some eligible failures may offer **one exact, approval-gated unsandboxed retry**.
It is not automatic. A nonzero payload can already have changed files or external
state, so review the partial-effect warning and verify the outcome before
approving any retry. Missing or invalid completion evidence means effects may
be unknown, not that the payload never ran. Output alone is not authorization
to replay an uncertain action.

`host-access` is a separate primary-user-only mode for intentionally running
local shell work outside the configured sandbox. Approval bypass is another
separate choice; neither should be used as an unexplained error workaround.
Record why host execution is necessary, keep the exception narrow, and restore
the intended policy afterward.

## Related pages

- [Approvals and review](approvals-and-review.md)
- [Project trust and instructions](project-trust-and-instructions.md)
- [Audit and diagnostics](audit-and-diagnostics.md)
- [Configuration](../configuration/README.md)
- [Normative security contract](../../SPEC.md#18-security-and-safety)

## Next step

Inspect the pane's effective boundary and scopes before approving work that
reads sensitive files, writes outside the project, or uses networking.
