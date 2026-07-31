# Sandbox mechanism

Mezzanine separates **permission decisions** from **operating-system
confinement**. Permission rules decide whether an agent action may run; the
sandbox decides what a permitted local shell action can access. Enabling a
sandbox never bypasses approvals, and approving an action never expands the
sandbox's configured authority.

This document explains the user-visible security boundary. The normative
contract is in [SPEC.md](../SPEC.md); exact fields and defaults are in the
[configuration reference](configuration-reference.md).

## Backends and bypasses

`permissions.sandbox` selects one of two backends:

- `policy-only` is the default. It classifies actions for approval and audit,
  but does not provide filesystem or shell-network confinement. Configured
  scopes remain advisory metadata in this mode.
- `bubblewrap` launches each eligible local shell workload in a constrained
  Linux Bubblewrap namespace. Its mounts and network namespace enforce the
  resolved maximum authority.

The `full-access` approval policy suppresses fresh whitelist prompts but still
uses the selected sandbox. In contrast, `host-access` is a primary-user-only
explicit bypass: local shell actions run on the host outside Bubblewrap while
hooks and explicit deny rules remain active. It is visible in status and is
not privilege separation between same-UID processes.

## How a Bubblewrap action is prepared

For each local shell action, Mezzanine follows this sequence:

1. It evaluates command, destructive-operation, and network policy. The
   normal approval flow must settle before the workload can run.
2. It resolves the pane's effective filesystem authority. Explicit
   `read_scopes` and `write_scopes` are maximum authority. If both are absent,
   a pane inside a trusted project receives that project's canonical root as
   read-write authority; the deepest matching trusted root wins. A pane with
   neither source has no Bubblewrap filesystem authority. Configured paths
   unavailable on this pane are omitted with an agent-pane warning; other
   successfully resolved paths remain active.
3. It probes the configured Bubblewrap executable in the target pane
   environment. Successful capability evidence is bound to the exact pane,
   environment signature, configuration generation, executable, and runtime
   profile. Missing, stale, truncated, failed, or timed-out evidence fails
   closed.
4. It compiles a typed launch plan from already-authorized paths. Command
   patterns, approvals, presets, and discovered operands cannot grant mounts.
   Complete declared command effects may narrow the maximum mounts; unknown
   effects retain the bounded maximum. `apply_patch` retains all effective
   write scopes so the semantic patch helper has its required authority.
5. It starts the child through the pane shell with the compiled plan. The
   launch never accepts raw Bubblewrap arguments, arbitrary binds, inherited
   environment allowlists, or user-selected sandbox destinations.

Preparation and probes occur in the target pane rather than a hidden host-side
executor, so executable and shell-environment evidence corresponds to the
workload that will run.

## Filesystem and process view

Bubblewrap projects only the authority needed by the compiled plan:

- Read scopes are mounted read-only; write scopes are mounted read-write and
  also imply read access.
- Paths are canonicalized in the pane environment. A configured path that is
  absent or unavailable on this pane is omitted with an `agent warning:` and
  Bubblewrap continues with reduced access. A mapping that would broaden
  authority, including a symlink escape, invalid authority, direct
  credential-directory authority, or the multi-user `/home` root, is excluded;
  launch fails only when exclusion cannot be proven.
- When a deterministic `/home/<user>` scope is permitted, sensitive existing
  descendants such as `.ssh`, `.gnupg`, `.aws`, `.azure`, `.kube`, `.docker`,
  and Mezzanine configuration are masked by private tmpfs mounts. They are not
  mounted merely because a parent is mounted.
- The child receives a synthetic home at `/home/<pane-user>`, named after the
  active pane user but backed by a private Mezzanine-managed directory rather
  than the host user's home. For trusted projects, Mezzanine may reuse a private managed home keyed
  by canonical project root and sandbox runtime profile. Its HOME and XDG
  paths remain inside that managed home. The child uses the invoking user's
  native UID and primary GID while inheriting the active pane shell's
  supplementary credentials. `permissions.bubblewrap.group_whitelist`
  maps selected active pane group names into the sandbox's synthetic group
  records; empty maps no supplementary names but does not filter inherited
  credentials. Mezzanine omits configured names absent from pane bootstrap
  evidence, logs a warning, and invokes the pane-local Bubblewrap executable
  directly without a privileged helper. An unprivileged user namespace may present unmapped host
  GIDs as the overflow GID. Group mapping does not expose sockets, devices, or
  paths by itself.
- `permissions.bubblewrap.env_whitelist` names optional variables to read from
  the active pane process through a bounded framed protocol. Effective values
  are passed as direct `--setenv` arguments and retained only in protected
  launch state; status and diagnostics expose names and omission classes only.
  Missing, invalid, oversized, or reserved mappings warn and degrade without
  disabling Bubblewrap or substituting controller-local values. Ordinary
  actions use this configured-forwarding profile. Internal semantic
  `apply_patch` phases instead use a deterministic no-forwarding profile and do
  not start pane-environment evidence transactions. Capability probing and
  workload compilation must use the same profile and evidence digest.
- The default runtime environment is rebuilt from a fixed non-secret set with
  a minimal PATH. Debian-style executable alternatives at `/etc/alternatives`
  are projected read-only so system compiler symlinks resolve. Host system and
  global Git configuration are disabled. A
  configured paired Git author name and email may be projected, but credential
  helpers, signing keys, includes, hooks, URL rewrites, and other host Git
  settings are excluded.

Selected built-in toolchains and constrained custom toolchains are additional
read-only projections at fixed sandbox paths. They are an allowlist, not a
general mount or PATH configuration mechanism. Toolchain roots add only their
validated read authority and do not grant write access at their host paths.

## Network boundary

`permissions.network_policy` controls the Bubblewrap profile for local shell
actions:

- `deny` uses an isolated network namespace for every shell action.
- `allow` uses an explicitly connected profile for every shell action.
- `prompt` uses the connected profile only after the action's network use is
  authorized.

The host TLS certificate store is mounted read-only in every profile. This is a
binary connected-or-isolated boundary; Mezzanine does not provide
destination-level filtering. Brokered `web_search`, `fetch_url`, and MCP
actions are not child shell processes: they use product-owned transports and
remain subject to their own controller capability and approval gates.

## Fail-closed behavior and fallback

Bubblewrap does not silently fall back to the host. Probe, setup, launch, and
pre-payload failures stop the action. For an action whose retained permission
decision was `prompt`, one normal approval may be offered for one exact
unsandboxed retry. The retry is never automatic, is tied to that turn and
action, and is consumed once. It warns that partial effects may already exist
when the payload might have started.

Payload lifecycle evidence is kept separate from command output. A validated
exit-code event proves the child ran; clean closure without that event proves
only that execution was not established. A non-zero payload exit is treated as
a command failure unless bounded assessment positively identifies a sandbox
failure.

## Inspecting and changing state

Use the following direct-user commands:

```sh
# Read-only configured and effective projection, readiness, and diagnostics.
mez sandbox status --verbose

# Preview guided setup without changing configuration.
mez sandbox plan

# Inspect and manage project trust used by trusted-project authority.
mez sandbox trust list
```

`mez sandbox status` does not migrate configuration, change trust, create a
managed home, or populate capability-probe caches. Its diagnostics use stable,
non-sensitive restriction identifiers instead of raw Bubblewrap arguments,
environment values, or unrelated host paths. Guided setup (`plan`, `enable`,
`preset apply`, and `disable`) requires direct-user confirmation; noninteractive
changes require `--yes`. Agents cannot change sandbox backend, authority,
Bubblewrap, trust, or bypass settings through model-authored configuration.

The agent-shell `/sandbox` and `/permissions` commands show the effective
pane-local state. `/sandbox enable --yes` and `/sandbox disable --yes` change
only the current pane unless `--global` is supplied. See the configuration
reference for presets, profile import/export, managed-home maintenance, and
toolchain commands.
