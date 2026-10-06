# CLI reference

## Purpose

Provide the command-line entry points for starting, selecting, inspecting, and
administering Mezzanine sessions and local services.

## Prerequisites

Install `mez` and use an interactive terminal when creating or attaching a
primary client.

## Invocation and global options

```text
mez [GLOBAL OPTIONS] [COMMAND [ARGUMENTS...]]
```

Global options are `--json`, `-S PATH`, `-L NAME`, `--iroh-profile NAME`,
`--iroh-invite-file PATH`, and `--save-as NAME`; they may appear before or after
a command. `--save-as` requires an invitation target and selects the
client-local alias used for later reconnects. `--json` selects machine-readable
output for structured command results; interactive attachment remains a terminal
interface. `-S` requires an absolute control-socket path, and `-L` selects a named
socket in the Mez runtime directory. The Iroh selectors are explicit remote
targets, conflict with Unix socket selectors, and never fall back to Unix after
a remote failure. Without a subcommand, `mez` uses the persistent local host
when available; otherwise it attaches to the first local session that accepts
a primary client or starts a session if none is available. Use `mez new` to
always start a new session. With a host target, omitted-target `mez attach` may
also create a session; use an explicit target or `--default` to avoid creation.

## Session commands

| Command | Behavior |
| --- | --- |
| `mez new [--dry-run] [--name NAME] [--x11\|--x11-trusted]` | Create a fresh local or explicitly selected remote session and attach; an interactive terminal is required unless using local `--dry-run`. `--name` assigns a session name. X11 modes require an explicit host-scoped Iroh target; takeover is not supported for fresh creation. Alias: `new-session`. |
| `mez serve` | Start a foreground session service; it does not attach a primary client unless `--attach-primary` is supplied from an interactive terminal. Alias: `daemon`. |
| `mez list [--all]` | List resumable sessions known to the local client. With the persistent local host, `--all` adds visible remote durable leases to the same scope-tagged aggregate. Alias: `list-sessions`. |
| `mez attach [SESSION_ID] [--observer\|--observe\|--default] [--x11\|--x11-trusted] [--x11-takeover]` | Attach a primary client, request read-only observer access, select an existing host default without creating, or request X11 forwarding for an authenticated Iroh primary. `--observer` and `--observe` are equivalent. `--default` conflicts with an explicit target; X11 options are described below. Alias: `attach-session`. |
| `mez detach [--client-id ID] [--session-id ID]` | Detach the selected client. From an interactive attachment, use `Ctrl+A d` to detach that invoking client. Paired host-profile administration requires both stable IDs and an active broker; `--session-id` is remote-host-only. Local/legacy detach retains its existing selection. Alias: `detach-client`. |
| `mez kill [session-id] --force` | Terminate the selected live session through its control socket; the optional target accepts a registered session id or creation-order index. `--force` confirms the destructive operation. Alias: `kill-session`. |
| `mez snapshot` | Manage persisted snapshots. With no subcommand it lists snapshots; see the snapshot forms below. |
| `mez host` | Serve, inspect, stop, or reconcile the persistent multi-session host. |
| `mez lease` | Inspect, release, revoke, or garbage-collect persistent-host leases through local administration. |

Creating or attaching a primary client needs an interactive terminal. `mez
serve` can run without one. Observer attachment also requires an interactive
terminal and immediately creates a read-only client bound to the current layout
owner. A session accepts up to 16 independently attached primaries. Each has
its own navigation and presentation; one layout owner determines shared pane
sizes, so another client's resize does not independently resize the processes.
`mez snapshot resume <snapshot-id> --serve` restores a snapshot as a foreground
daemon; add
`--attach-primary` only when the invoking terminal should also attach. Use `mez
--help` and `mez <command> --help` for the current argument and target syntax.

## Foreground service options

### Harness bootstrap

`mez bootstrap <harness> [--vendor-version VERSION] [--root ABSOLUTE_ROOT]`
accepts `claude`, `codex`, `copilot`, `opencode`, `cursor`, and `pi` as research
candidates, not certified integrations. The default or `--plan` inspects a plan;
`--check` checks owned state. Explicit `--apply`, `--uninstall`, and `--recover`
are separate intents. The command is daemon-free and does not perform socket
cleanup, vendor executable discovery, credential installation or hook-trust bypass.

The Gemini external harness is retired, with no replacement. Every Gemini
bootstrap intent is rejected before root access. New canonical `gemini` launch
and external usage admission are also rejected. Historical expense and replay
guards remain intact; independent Google/Gemini provider and model usage under
native or other harnesses is unaffected. No vendor files or user hooks are removed.

**No released vendor adapter is currently certified in the compiled registry.**
Candidate plans report `supported=false` and unavailable lifecycle/usage;
mutation requests fail before touching the root. Test-only manifests qualify the
common engine, not any vendor release. Vendor adapter tasks must supply reviewed
release-specific artifacts, private launch binding, neutral responses and recovery
guidance before installation is enabled.

The Codex `0.160.0` component currently projects bounded main-session lifecycle
identifiers from its released hook schemas. It discards prompt/transcript/tool
content and child-context events, and does not infer token usage. This pure
component does not deliver launch capabilities, renew idle leases, install hooks,
or certify a live Codex process. Its release reference is
`openai/codex` tag `rust-v0.160.0`, `codex-rs/hooks/schema/generated`.

The OpenCode `1.17.13` component projects settled assistant-message snapshots
only for an explicitly bound session. Its released producer separates uncached
input and non-reasoning output; the projection restores inclusive counters with
checked arithmetic and rejects ambiguous numbers. Message IDs are snapshot/upsert
identities, not permission to add every repeated event. Completed messages map to
one immutable durable delta stream per bound session/message: identical replay
after reconnect adds nothing; changed completed counters, model or completion
time conflict rather than silently charging another revision. Partial snapshots
are not charged. No callback-local sequence is treated as a vendor revision.
This component does not
install a plugin, bind a shared server, deliver credentials or certify live usage.
Release references are `anomalyco/opencode` tag `v1.17.13`,
`packages/schema/src/v1/session.ts` and `packages/opencode/src/session/session.ts`.

The Pi coding-agent `1.0.2` component projects content-free lifecycle facts only
for an explicitly bound context session. `agent_end` and `turn_end` are not final
settlement; `agent_before_settle` supplies only a provisional outcome, and
`agent_settled` reports final settlement without inventing a successful outcome.
Extension UI prompts indicate input wait, not automatic approval wait. Shutdown
reasons distinguish quit from reload/new/resume/fork, not prove process death.
This component does not install an extension, deliver credentials, renew leases,
rebind sessions or certify live usage. Sources are the installed
`@earendil-works/pi-coding-agent` `1.0.2` extension declarations and agent-session
producer. Pi work is independent of the retired Gemini harness, not its replacement.

Offline callback qualification is available with explicit trusted local paths:
`MEZ_PI_NODE=/absolute/node MEZ_PI_PACKAGE=/absolute/pi-coding-agent timeout 180s cargo test -p mezzanine --lib --quiet pi_lifecycle_released_loader -- --ignored`.
This runs only the transport-free observer through the pinned package's inline
extension loader and runner, then checks emitted facts against the Rust projector.
It uses an empty child environment and temporary home, does not discover user
extensions, request credentials, start providers or install configuration, and
does not certify private launch delivery, renewal, reload rebinding or accounting.
Package-independent tests also run via `timeout 120s node --test scripts/test-pi-observer.mjs`.

Real inherited-descriptor qualification uses the same explicit trusted paths:
`MEZ_PI_NODE=/absolute/node MEZ_PI_PACKAGE=/absolute/pi-coding-agent timeout 180s cargo test -p mezzanine --lib --quiet pi_session_released_extension_uses_inherited_descriptor -- --ignored`.
It passes a Unix socket as descriptor 3 to an isolated Node child, drives the
released inline extension loader/runner, and checks ordered lifecycle delivery
through the Rust callback bridge and session coordinator. Daemon capabilities
remain in the parent; the child receives no token in arguments or environment.
The daemon responses are synthetic and callbacks are manually emitted. This
qualifies descriptor plumbing, not a production launcher, vendor event producer,
installed extension, reload/session replacement or token accounting.

The pure Pi launch-owner reducer retains at most 32 lifecycle reports (one slot
reserved for retirement). Exact pending identities survive observer reload;
only the originating owner's head acknowledgment consumes a report. Old observer
epochs cannot publish after replacement. Provisional outcomes do not publish
success before settlement, and repeated settlement preserves its accepted
outcome. Queue/counter exhaustion is explicit and leaves accepted state intact.
The reducer has no transport, credentials, renewal timer or installation authority.

The internal Pi capability-only transport supports restricted registration,
renewal, presentation and retirement over same-user-authenticated Unix IPC. It
requires an explicitly supplied private capability and originating lifecycle
owner; it cannot initialize a primary, mint authority or rebind a session.
Each exchange has one 500 ms deadline and strict reply bounds. Matching typed
acknowledgments consume only the original pending head; lost/failed replies
retain exact work without automatic retries. Errors omit tokens and peer content.
The internal renewal worker registers once and renews independently of callback
traffic at half its conservatively observed remaining lease. It requires the same
registration identity and increasing server expiry; a monotonic expiry fence
accounts for reply latency and integer-second rounding. Failure, cancellation or
worker drop clears local availability without automatic retry or rebind. A lost
renewal reply is not proof that no remote renewal occurred.
Private launch delivery, worker/extension integration and installation remain
unfinished; these components do not enable a certified manifest.

The internal session coordinator combines lifecycle delivery and idle renewal
without spawning detached tasks. Callback ingress is nonblocking and limited to
32 typed observations or explicit reload-attachment requests. Presentation waits
for acknowledged registration and stays bounded by the current conservative
lease; renewal continues while delivery is pending. Old epochs are ignored,
same-session reload attachment is explicit, and cancellation or failed replies
preserve pending report identity. Closing ingress ends worker ownership. Actual
closure fences registration and outstanding delivery without draining buffered
facts. Reload attachment first offers a proposal; an unread or dropped proposal
leaves the observer suspended. Explicit confirmation activates the replacement;
losing its result does not undo an accepted activation command. Actual
extension IPC, private launcher delivery and installation remain unfinished.

The internal inherited-stream bridge connects content-free observer facts to
coordinator ingress. The launcher supplies a connected same-user Unix stream,
session and epoch; none is selected by a callback payload. Exact JSON lifecycle
frames are limited to 1,024 bytes, with a 250 ms total partial-frame deadline
and no idle-silence timeout. The JavaScript sink bounds buffered writes to
32 KiB and stops accepting telemetry after backpressure or error without
changing vendor decisions. Daemon capabilities never cross this observer link.
The launcher retains session lifetime separately from observer stream closure.
Private launcher delivery and installable extension wiring remain unfinished.

The injectable Pi extension factory wires the observer to its stream sink only
at session start. Factory loading opens no resources; a matching context session
can open its explicitly supplied channel once, and shutdown forwards an inert
teardown fact before idempotent cleanup. Failures remain neutral without retry or
automatic rebinding. Offline tests exercise this wiring through the installed
Pi 1.0.2 loader/runner; actual private launcher delivery, installation and
supported-platform certification remain unfinished.

The internal observer launch primitive now performs the inherited-descriptor
handoff used by the offline loader fixture. Its caller supplies an absolute
executable and directory, exact arguments, environment and stdio; no ambient
environment or executable discovery is used. Descriptor 3 carries observations
only. The process owner must reap the child, independently of observer disposal
or telemetry failure. This is not a public launch command or an authorization
issuer: production session selection/private binding, installation and platform
certification remain unfinished, and no Pi manifest is enabled.

The internal owned observer runner joins strict callback ingress and session
lease delivery without detached tasks or child ownership. Clean stream EOF drains
accepted facts before ending renewal; it does not imply process death or request
deregistration. Explicit quit facts retain their existing exact-session retirement
semantics. Errors and cancellation preserve pending reducer evidence without
automatic reconnect or vendor replay. Production authorization, reload/replacement
orchestration and installation are still separate, unfinished work.

An internal compiled artifact candidate owns five files under
`extensions/mezzanine/` in an explicit agent-directory root. A package manifest
selects one entry point; private `.mjs` siblings are not independently discovered
as extensions. The entry is registration-only during loading and opens descriptor
3 only at matching session start with explicit observation markers and a socket
check. These markers carry no daemon capability. Temporary-root repeat, conflict
and uninstall checks and an offline Pi 1.0.2 discovery/runner fixture qualify the
candidate, not a public installation: the certified registry remains disabled.

The common engine owns exact whole files or exact object entries in strict JSON.
Edited ownership conflicts rather than overwriting user changes. Repeat is
byte-stable; uninstall removes owned entries rather than restoring stale backups.
Strict-JSON changes may reformat surrounding whitespace; JSONC/TOML are not
silently converted. Publication uses no-follow directory handles, bounded regular
files, a cooperating-installer lock, exact preimages and a private forward-recovery
journal. A partial transaction remains explicit; recovery refuses foreign edits.
Final check plus rename is not atomic CAS against arbitrary external writers.

### Normalized observational hook helper

`mez -S /absolute/control.sock harness-event` reads one JSON envelope from stdin:

```json
{"operation":"presentation","launch_token":"<private capability>","generation":1,"external_session_id":"bound-run","data":{"sequence":1,"state":"running","title":"Task"}}
```

The token is stdin-only, never an argument or persisted configuration value.
An attached primary must first authorize the exact pane-root launch. The helper
maps only `register`, `renew`, `end`, `presentation`, and `usage` to their restricted
external-agent operations; `data` uses the documented RPC fields without identity
overrides, arbitrary methods, paths, prompts, transcripts or nested vendor payloads.
It authenticates the Unix daemon's same-user peer and does not initialize a client.
Input is capped at 64 KiB with a 250 ms EOF deadline; the single exchange is bounded
to 500 ms. No retries or subprocesses are issued. Telemetry errors produce neutral
`{}` output and success exit, without echoing credentials or daemon errors.

This is not vendor hook installation or certification. Adapters must privately
deliver launch credentials, normalize content-free released payloads, serialize
presentation sequences, renew leases during idle periods, and verify that `{}` is
neutral for the pinned vendor event. An absent daemon loses telemetry without
changing approvals, sandbox policy, focus or continuation behavior. Remote/shared
server sessions require explicit binding; inherited pane variables alone are not
authorization. Iroh targets are unsupported by this helper.

`mez serve`, `mez snapshot resume --serve`, and `mez snapshot resume-latest
--serve` accept the same service options:

| Option | Behavior |
| --- | --- |
| `--message-socket PATH` | Bind the local message service at an explicit absolute path. |
| `--event-socket PATH` | Bind the local event service at an explicit absolute path. |
| `--no-aux-sockets` | Do not bind the default message and event sockets. |
| `--attach-primary` | Attach the invoking interactive terminal as the primary client. |
| `--max-control-connections N` | Limit concurrent control connections. |
| `--max-message-connections N` | Limit concurrent message connections. |
| `--max-event-connections N` | Limit concurrent event connections. |
| `--max-event-batches-per-connection N` | Limit event batches served on one event connection. |

By default, a foreground service derives separate message and event socket
paths from the selected control socket. Explicit socket paths must be absolute,
and every connection or batch limit must be greater than zero. A message or
event connection limit requires the corresponding auxiliary socket to be
enabled. Use `--no-aux-sockets` for an intentional control-only service.

The built-in Unix attach client discovers the event service at the standard
path derived from the control socket; it has no separate event-socket selector.
A nonstandard explicit `--event-socket` path is therefore useful only to a
custom client configured for that path; the built-in `mez attach` client will
not discover it. Without a reachable event socket, control requests still work,
but an idle attachment does not receive redraw wakeups and may not fetch a
fresh view until input, focus, mouse, or resize activity occurs.

## Snapshot forms

Snapshots preserve layout, pane sizes, and landing navigation. They do not preserve
running processes, live client identities, layout ownership, transient
presentation, terminal history, or agent conversations. Pending approvals and
approval grants do not become authority in a restored session:

| Command | Behavior |
| --- | --- |
| `mez snapshot` or `mez snapshot list` | List persisted snapshots. |
| `mez snapshot create [-n NAME]` | Create a snapshot of the live session selected by the control socket. |
| `mez snapshot inspect <snapshot-id>` | Inspect one saved snapshot. |
| `mez snapshot delete <snapshot-id>` | Delete one saved snapshot. |
| `mez snapshot resume <snapshot-id>` | Reconstruct the saved layout model without starting a daemon; add `--serve` to launch fresh panes in a foreground daemon. |
| `mez snapshot resume-latest [--session-id ID]` | Reconstruct the newest matching layout model without starting a daemon; it also accepts `--serve`. |

**Current store limitation:** live `snapshot create` writes the selected
daemon's `layouts` store, while the offline commands above read `snapshots`.
Consequently, listing or resuming offline does not find a layout just created
through that live request. Use command-prompt `save-layout --name NAME` and
`load-layout --name NAME` for live layouts; loading replaces topology and starts
fresh pane processes. Offline inspection and resume apply only to snapshots
already in the offline store. See [recovery guidance](../operations/lifecycle-detach-and-recovery.md#snapshot-and-resume-deliberately).

Both restore commands accept `--restart-command <command>`. Use it with
`--serve` when restarted pane processes must remain alive; without `--serve`,
the reconstructed runtime is transient and terminates those processes before
the command exits. A live restore starts fresh processes and cannot reconnect
to the processes that existed when the snapshot was taken.

## Configuration, identity, and integrations

| Command | Subcommands and scope |
| --- | --- |
| `mez config` | `init`, `path`, `default`, `validate`, `get`, `layers`, `set`, `unset`, and typed `model list\|add\|update\|remove\|sync`. Mutations write the user configuration by default; `--scope project` targets an eligible trusted project overlay and `--file PATH` selects an eligible file in that scope. |
| `mez auth` | `status`, `login`, and `logout` for provider credentials and metadata. |
| `mez mcp` | `list`, `inspect`, `login`, `logout`, `status`, `add`, `remove`, `enable`, `disable`, `set`, `unset`, `tools`, and `approval` manage configured MCP servers, stored MCP credentials, tool filters, and server approval settings. |
| `mez sandbox` | Inspect sandbox status, plan or enable the platform backend, disable confinement, manage presets and profiles, inspect managed-home caches, and manage project trust. If the required backend is unavailable, enablement fails without changing settings. `mez sandbox trust` supports `list`, `inspect PATH`, `add PATH`, `reject PATH`, and `revoke PATH`. |
| `mez issue` | Add, show, update, query, and delete local project issues. |
| `mez memory` | List, inspect, add, edit, delete, archive, mark stale, restore, record use or confirmation, supersede, prune, export, and search persistent memory records. |
| `mez session-catalog` | Inspect and rebuild the saved-session discovery catalog. |
| `mez storage export STORE` | Export `memory`, `sessions`, `leases`, `assignments`, `history`, `project-trust`, or `snapshots` as TSV for inspection. Output remains TSV even with `--json`; a missing store is an error. |
| `mez remote` | Use authenticated local Unix control for `status`, `invite`, `clients`, `rename CLIENT_ID LABEL`, and `revoke CLIENT_ID [--reason TEXT]`. Client-local commands are `pair --invite-file PATH [--name NAME]`, `invitation inspect PATH`, and `profile list\|show\|rename\|remove\|check`. Paired Iroh clients cannot use server trust-administration methods. |
| `mez completion <shell>` | Generate a completion definition for `bash`, `elvish`, `fish`, `powershell`, or `zsh`. |

`mez config model` manages one configured provider's reusable model records by
model ID. `add` accepts `--display-name`, comma-separated `--aliases`, token
limits, comma-separated `--reasoning-levels` and `--capabilities`, and repeated
non-secret string `--provider-option KEY=VALUE` values. `update` is selective
and also provides explicit `--clear-*` and `--remove-provider-option` controls.
`remove` and `update --new-id` refuse to invalidate `default_model` or
`model_profiles` references. Use global `--json` for stable machine-readable
output. An empty compatible-provider list explains how to add a model or use a
supported live catalog endpoint.

`mez config model sync PROVIDER` fetches only that provider's raw live model
catalog and compares it with explicit configured records. It does not use the
session catalog cache or configured and built-in runtime fallback models.
Synchronization previews without writing by default:

```console
mez config model sync PROVIDER
mez config model sync PROVIDER --apply
mez config model sync PROVIDER --prune
mez config model sync PROVIDER --prune --apply
```

`--apply` is the explicit persistence switch. `--prune` is independent: it
includes configured-only records in the removal plan, while omission retains
them. Referenced records block the complete prune application, including
provider defaults and model profiles that select an alias. Live metadata fills
only omitted display, reasoning, token-limit, and capability fields; explicit
configured values, explicit empty lists, aliases, and provider options win and
conflicts appear in the output. Project targets may use an inherited provider
connection but receive only their own model overrides. Failed or unsupported
catalog discovery does not alter the target and directs the caller to
`mez config model add PROVIDER MODEL_ID`.

This command is distinct from `/refresh-provider-info`: the slash command
updates only the running session's best-effort catalog cache and retains runtime
fallback behavior, while `config model sync` is the deliberate durable path.

### Iroh targeting and pairing

Pairing starts on a configured, running host and finishes on the client after
the invitation file is transferred through a confidential channel:

```console
# Host, through local Unix control
mez remote invite --role primary --allow-create --output mez-invite.json

# Client, after confidential transfer
mez remote invitation inspect mez-invite.json
mez remote pair --invite-file mez-invite.json --name home-mez
mez --iroh-profile home-mez attach
```

Use an `observer` invitation when the device needs read-only attachment, and
omit `--allow-create` when it must attach only to an explicitly named existing
session. See [Remote pairing and
recovery](../safety-and-trust/remote-pairing-and-recovery.md) for listener
prerequisites, route policy, role ceilings, revocation, and identity recovery.

`mez remote invite` accepts `--role observer|primary`, `--expires SECONDS`, and
`--output PATH`. A role ceiling does not grant session creation. Add
`--allow-create` when the device may use remote `new` or omitted-target
`attach`; optional `--max-leases`, `--max-live-sessions`, and
`--lease-lifetime-ceiling` narrow that authority. `--allow-kill` separately
permits a primary device with creation authority to force-kill sessions it
created.

An explicit Iroh primary `attach` accepts these X11 options. Remote `new`
also accepts `--x11` and `--x11-trusted`, preserving fresh creation and its name;
`--x11-takeover` is attach-only because a fresh session has no route to replace.

| Option | Behavior |
| --- | --- |
| `--x11` | Request X SECURITY untrusted forwarding. Forwarding remains off unless requested, and this mode fails closed if a local untrusted credential cannot be prepared. |
| `--x11-trusted` | Request full trusted X11 forwarding. This conflicts with `--x11`, requires `transport.iroh.x11.allow_trusted = true` on the host, and fails closed unless local `xauth` can issue a fresh trusted authorization. |
| `--x11-takeover` | Explicitly replace another attachment's X11 route. It requires either `--x11` or `--x11-trusted`. |

These flags require an authenticated Iroh primary and are rejected for
observers and Unix targets. An unsupported peer or denied host policy is a
visible initialization failure; the client does not reconnect without the
requested forwarding. The attaching machine accepts conventional Unix
displays, constrained XQuartz launchd sockets, and TCP displays. A TCP hostname
or address—including a non-loopback target—is resolved once and frozen with
its real cookie before dialing. Neither value is sent to the server.

For example, `mez --iroh-profile home-mez new --name work --x11` creates and
attaches a fresh host session with untrusted forwarding. First-use host pairing
can use `mez new --iroh-invite-file HOST.json --save-as home-mez --name work --x11`.
Local X11 requests and remote `--dry-run` are rejected before session allocation.

Direct control commands keep Unix as their default target. `--iroh-invite-file
PATH` explicitly performs first-use pairing from an owner-only, bounded JSON
invitation file, while `--iroh-profile NAME` explicitly uses a protected paired
profile. Add `--save-as NAME` to save invitation-issued authority under a
human-readable client-local alias. The alias is not a trust input: the pinned
server identity, client endpoint identity, role ceiling, and protected device
credential remain authoritative. These selectors conflict with `-S` and `-L`,
never fall back to Unix, and apply to the supported remote session commands:
`new`, `list`, `attach`, `kill`, and `detach`. Host-profile attach and kill
targets accept a lease ID, stable session ID, or exact name. Remote kill
requires an explicit target, `--force`, a primary role ceiling, and separately
granted force-kill authority.

An enabled host-scoped Iroh configuration does not change bare `mez` or `mez
serve` into remote-listener commands. Those direct session commands use Unix
control; only `mez host serve` binds the host Iroh endpoint. Use an explicit
`--iroh-profile` or `--iroh-invite-file` selector with a supported remote
command to initiate Iroh client transport.

Explicit Iroh targets work when listener-oriented `transport.iroh.enabled` is
false. They require `transport.iroh.outbound_enabled = true` (the default) and
derive a client-only direct or relay policy from the target's pinned address;
they do not start a listener or enable port mapping. Invitation targets perform
no address lookup. Paired profiles may use an address-lookup service only when
the user explicitly configured one; a successful endpoint-ID-pinned reconnect
refreshes authenticated route hints in the protected profile.

`transport.iroh.compression_codecs` controls the allowed compression choices
for remote clients and listeners: `zstd-stream`, `lz4-stream`, `zstd`, `lz4`,
and `none`. Streaming codecs are opt-in and take precedence over their
non-streaming alternatives when both peers support them. Include `none` only
if uncompressed connections are acceptable; there is no automatic downgrade
to it when it is absent. To disable compression for diagnosis or rollback, set
`compression_codecs = ["none"]` and restart the affected processes.
X11 forwarding uses the same negotiated compression and has no separate flag.

### Persistent-host command contract

The persistent host manages multiple sessions behind one local or remote
endpoint. The direct `mez serve` endpoint serves only its own session; attaching
to it does not create another session.

The persistent-host command surface is:

```text
mez host serve [--max-sessions N] [--max-live-sessions N]
mez host status
mez host stop [--timeout SECONDS]
mez host reconcile

mez lease list [--state STATE] [--owner CLIENT_ID] [--all]
mez lease show <lease-id|session-id|name>
mez lease release <lease-id|session-id|name> [--terminate]
mez lease revoke <lease-id|session-id|name> [--reason TEXT] [--terminate]
mez lease gc [--older-than DURATION] [--dry-run|--apply]
```

The `mez host serve` limits override the configured maximum durable session
records and concurrently live session runtimes for that invocation. Both must
be positive.

Lease administration uses only the protected local host socket. Active release
or revocation requires `--terminate`; neither operation revokes device trust.
Garbage collection previews by default, removes only terminal lease tombstones,
and requires `--apply` to mutate durable state. Durations accept plain seconds
or `s`, `m`, `h`, and `d` suffixes. Remote durable leases reserve and authorize
sessions but do not checkpoint or reconstruct remote runtimes after host restart.

In that mode, bare local `mez` uses the protected local host, attaches to an
eligible session, or immediately creates and attaches when none is eligible.
`mez new [--name NAME]` always creates. Local commands do not require Iroh or
pairing. `mez serve` remains the foreground single-session compatibility path;
`mez host serve` is the sshd-like foreground service for a service manager.
It writes its initial machine-readable readiness record to standard output and
writes local and remote client connection, rejection, timeout, and failure
diagnostics to standard error. Local records identify the authenticated Unix
peer UID and request method. Remote Iroh connection, disconnection, and
post-authentication failure records identify the client by authenticated
endpoint ID and a privacy-safe route category (`direct`, `relay`, `custom`, or
`unknown`). Capacity and recurring maintenance degradation are logged only on
state transitions so routine operation remains quiet.

An Iroh profile in persistent-host mode identifies one stable host rather than
one session. The remote forms are:

```text
mez --iroh-profile HOST attach
mez --iroh-profile HOST attach <lease-id|session-id|name>
mez --iroh-profile HOST attach --default
mez --iroh-profile HOST new [--name NAME]
mez --iroh-profile HOST list
mez --iroh-profile HOST kill <lease-id|session-id|name> --force
```

Omitted-target `attach` selects the existing host default or creates
one when none exists. `attach --default` selects an existing default and never
creates. `new` requests a fresh session, while an explicit
attach target selects only an authorized existing lease. Pairing and profile
checks are implemented as host-only operations and cannot create or attach a
session.
CLI mutation keys use a fresh random nonce per logical operation rather than a
process ID. Prepared request retries retain their original key; starting a new
command is a new operation, not recovery of an ambiguous earlier creation.
An internal shared endpoint resource now retains one protected identity across
bounded independent connection leases. Frontend IPC and CLI consumer migration
remain unfinished: this component does not yet permit simultaneous CLI processes
to share the paired identity. The existing exclusive identity lock remains enforced.
Shutdown waits retain their original future across cancellation or timeout. An
abandoned resource with unproven teardown withholds identity reuse until its
owning process exits; another endpoint is never started as a cleanup fallback.
The owner retains its configuration-root directory identity. Discovery checks
are read-only and reject a relocated, replaced or no-longer-private root.
An internal local admission component authenticates Unix peers and accepts only
a bounded versioned hello under a finite deadline. Its exact frontend handles
retain endpoint ownership but grant no remote authority. Internal listener
publication uses private socket permissions and held-parent, identity-checked
cleanup; replacement entries are preserved. This is not an atomic bind or unlink
against hostile same-user renames. Startup election, remote forwarding and CLI
consumer migration remain unfinished.
Internal setup consumes an exact frontend handle and resolves its protected
profile inside the owner. Caller credentials are rejected; existing control
initialization and profile role/scope validation remain authoritative. A timed-out
profile waiter does not cancel blocking I/O: its finite slot stays occupied until
the worker exits. This preparation does not dial or create a remote session.
The next internal transition supports pinned direct transport only. It retains
the exact frontend and independent connection lease, verifies the server ID,
and negotiates a bounded codec without sending application initialization.
Relay/discovery policy qualification, remote authority settlement and CLI
activation remain unfinished; unsupported policy rejects without another endpoint.
An internal host-only initialization transition sends one owner-authenticated
request and validates correlated observer/host-only settlement. Private proof
and raw peer responses stay inside the owner; only allowlisted facts are retained.
An internal host-list exchange now returns bounded validated lease summaries
through exact-handle IPC using the fixed read-only host method. It exposes no
device proof or private principal/checkpoint metadata and retires only the
management connection. Real-host fixtures qualify listing beside a live sibling
without session allocation; ordinary CLI consumer migration remains unfinished.
Ordinary `list --iroh-profile NAME` now reuses an authenticated active broker,
after checking the current outbound veto. Only missing or refused discovery
retains direct fallback; unsafe discovery, protocol errors and failed broker
operations do not create a competing endpoint. Invitation listing and automatic
broker startup are unchanged; other ordinary consumers remain unmigrated.
Host-scoped `remote profile check NAME` also reuses an authenticated active broker.
Its authentication-only exchange requires no session-list permission and creates
no session. Outbound veto and unsafe/protocol/operation failures remain terminal;
only absent/refused discovery retains direct fallback. Legacy checks are unchanged.
An internal broker pairing operation now reads a protected invitation by absolute
path, checks host scope, expiry and alias pinning, and redeems once using the retained
endpoint. Issued proof is published privately before the closed success reply;
neither invitation nor device credentials enter frontend IPC. Blocking workers
retain endpoint/capacity ownership until they exit, even after waiter cancellation.
Uncertain results require profile inspection, not automatic redemption replay.
Ordinary host-invitation `remote pair` now reuses an authenticated active broker,
checking the current outbound veto before discovery. Only initial absent/refused
discovery retains direct pairing; connected discovery, protocol or operation errors
never acquire another endpoint or replay redemption. The closed client reply must
match the exact handle and expected profile alias. Host-invitation `attach`/`new`
also reuse an active broker: private pairing completes once, then fresh authenticated
IPC carries the original attachment intent/key. No post-pair failure permits direct
fallback or replay. Initial absent/refused discovery retains direct invitation setup.
Legacy pairing, automatic invitation startup and broker X11 remain separate.
Paired-profile `kill --force TARGET` reuses an authenticated active broker too,
preserving the exact target and invocation key through a fixed host kill request.
The host still enforces destructive authority and visibility. Only correlated
revoked-lease evidence reports success; uncertain exchanges never replay or fall
back to a competing endpoint. Invitation operations and attach/new remain separate.
A paired-profile attachment setup adapter now preserves prepared routing
and invocation keys through an existing authenticated broker, checking current
outbound policy and protected profile role. Missing/refused discovery leaves the
direct path eligible only before connection; discovery loss during connected
readiness is terminal. Readiness/setup failures never acquire a competing identity
or replay creation. Ordinary `attach` and `new` now reuse an active broker for
eligible paired host profiles, with guarded terminal output and restoration.
OS signals retire only the local frontend; terminal Ctrl-C remains forwarded input.
Initial absent/refused discovery now permits elected first-owner startup for
qualified pinned direct profiles, using the running binary and retaining the
exact child through setup and foreground exit. Startup failure does not switch to
direct setup or replay creation. Exited children are observed/reaped; live children
are not killed on frontend exit, and reaping remains best-effort after disposal.
Unqualified routes and absent-broker X11 retain direct eligibility before startup.
Broker X11 forwarding remains unfinished; X11 with an active broker rejects
before session setup rather than switching to a competing endpoint.
Errors after writing never automatically replay initialization. Session
creation/attachment uses a separate internal transition retaining the original
intent/key and validating exact client/session/active-lease evidence. A loopback
host fixture qualifies two sessions while the first remains attached and sibling
control survives retirement; this is not multi-process CLI acceptance. Event/X11
forwarding, startup election and CLI activation remain unfinished.
An internal self-detach API now consumes the exact initialized primary frontend,
preserves its mutation key and validates exact-client settlement. It exposes no
sibling target and ends the settled broker pipeline after reply delivery. Errors
are not replayed; ordinary detach-command migration and terminal restoration
remain separate from this retained-session API.
Optional version-one events now remain owned by the initialized session and its
exact connection lease. Setup validates the preface within a deadline; later
versions and X11 remain rejected. Real-host fixtures qualify event availability
after sibling retirement, not frontend forwarding or ordinary CLI activation.
A separate internal clipboard-session admission now requires explicit version-two
primary intent and true capability in the validated initialize reply before stream
acceptance. Internal supervision selects this path for explicit version-two intent
and retains typed effect ownership on the exact connection. Redraw-only polling
and foreground entry reject these sessions rather than discard effects. Real-host
fixtures qualify bounded Unicode item delivery across all codecs and sibling
survival after self-detach, not host clipboard writes or ordinary CLI attachment.
A separate internal X11 admission now retains exact correlated route authority
for an explicitly requested primary offer. Missing capability, changed trust mode
or invalid route proof rejects; proof stays outside local summaries. This staged
path starts no relay and does not enable ordinary supervisor or CLI X11 forwarding.
Bounded internal channel admission now authenticates the retained route's fixed
preface before exposing setup bytes. Pending/active streams share finite capacity,
and inbound stream credit is granted only after route settlement. Timeout or
cancellation releases the slot and resets only that stream; retained endpoint
ownership prevents premature identity reuse. This is not frontend forwarding or
local X credential provisioning, and ordinary X11 activation remains gated.
A consumed internal channel relay now validates the offered fake setup credential
and uses direction-local codecs for a caller-owned byte stream. Setup delivery is
deadline-bound; normal bidirectional completion preserves FIN tails, while failure
or cancellation resets only the owned channel. Five-codec synthetic fixtures qualify
half-close and rejection behavior, not real-cookie substitution, authenticated
frontend IPC, a physical X server, or ordinary CLI X11 activation.
A dedicated local-stream handshake now checks kernel UID and exact retained
frontend/session ownership. Version-two requests do not guess or select channel
occurrences; readiness reports the broker-assigned positive occurrence before
switching to raw bytes. Buffered premature bytes reject without silent loss. Socket-pair tests
qualify this boundary only: listener publication, occurrence allocation, real-cookie
substitution and ordinary broker forwarding remain separate integration work.
An internal session-owned composition now supplies the retained identities, fake
cookie and codec, reserves finite capacity and checked occurrences before awaits,
and releases the session borrow for concurrent control work. Owned reservations
retain endpoint lifetime but not an independent connection lease; parent retirement
cancels handoff/relay. One setup deadline covers local handoff,
remote preface and initial delivery. An integrated synthetic fixture qualifies
rejected-occurrence advancement and dedicated ping/pong/FIN delivery, preserving
read-ahead setup bytes after readiness, capacity rejection without occurrence loss,
and pending reservation disposal after parent retirement. This composition publishes
no listener; ordinary X11 activation remains unfinished.
Session-scoped reservation sources now share the same checked atomic allocator
and capacity pool while control dispatch owns the session. Concurrent allocation
tests verify distinct occurrences, saturation without watermark advancement,
exhaustion without permit leakage, and rejection after parent retirement. Sources
do not acquire independent connection leases or enable ordinary forwarding.
A separate attaching-client helper now validates bounded fake-cookie setup and
substitutes the real cookie only on its frozen local X connection. Setup read,
connect and initial delivery share a deadline; subsequent bytes and half-close
remain caller-owned. Synthetic local TCP fixtures qualify both byte orders,
rejection and cancellation, not a physical X server or ordinary CLI activation.
The prepared credential lease remains separately owned by the attachment caller.
A staged dedicated listener now publishes an owner-only socket under the retained
configuration root, preserves existing entries, and cleans only its recorded socket
through the held parent. Pending accepts and admitted streams share finite slots;
accepted streams retain endpoint ownership through relay. Kernel UID is checked
before handshake decoding. Publication remains a cooperative same-user pathname
boundary, and this component does not enable ordinary X11 forwarding.
An internal supervisor now owns bounded channel futures alongside the same
consumed control request. Stalled handshakes do not block control replies;
independent listener/channel capacity waits retain pending admission alongside
control instead of spinning or retiring the session. Peer authentication rejection
closes only that stream; root and listener failures remain terminal.
cancellation or control failure disposes channel work and listener publication.
Synthetic tests qualify control progress and cleanup, not ordinary listener
dispatch, occurrence announcement or CLI X11 activation.
An exact-session discovery exchange now returns only the dedicated listener's
validated basename while supervision owns publication. It allocates no channel
occurrence or remote permit and exposes no route proof. Closed client validation
rejects foreign ownership and directory paths; this does not open a channel or
enable ordinary CLI X11 forwarding.
A staged client opener now retains protected root/control/dedicated socket evidence,
checks kernel UID and exact version-two readiness, and preserves raw setup bytes
read ahead after that reply. Finite permits cover opening and active channel lifetime;
errors and cancellation release the dedicated stream without retry or endpoint
acquisition. Socket fixtures qualify this boundary, not ordinary CLI forwarding;
the attachment supervisor must dispose openers and channels when its parent ends.
Each internal initialized session now retains its own shared health tracker and
samples only its exact connection when due. Missing measurements remain unknown;
closed connections report disconnected with unknown quality. Sibling sampling
is independent. This boundary adds no worker, frontend health reply, local status
composition, or ordinary attachment activation.
An explicit internal health exchange now delivers only connected state and coarse
quality across exact-session IPC under a finite deadline. It exposes no paths,
counters or credentials and performs no remote request, rendering or receipt ACK.
Real-host sibling fixtures qualify this exchange, not ordinary CLI attachment.
An internal display-only view exchange now carries bounded rendered lines and
validated session identity across exact-handle local IPC. It does not provide
terminal input, presentation acknowledgement or the complete attached
renderer. Failed exchanges dispose their owner instead of replaying requests.
Snapshots retain optional server-resolved ordinary render-rate metadata without
inventing a ceiling when unavailable or zero. Invalid explicit values reject;
metadata retention alone does not enable foreground pacing or CLI activation.
Snapshots also retain optional validated view identity and event cutoff. These
are server revision evidence, not proof of local output commitment or permission
to reuse a conditional-rendering baseline. Invalid explicit metadata rejects.
An explicit internal conditional API now reuses exact identity/geometry only
after complete output and receipt settlement. The broker checks its delivered
base; unchanged replies carry no replacement rows or receipts. Replacement
snapshots and terminal resets invalidate reuse. Callers must invalidate before
external writes or writer replacement. The internal foreground uses this fence
for refreshes: unchanged replies reschedule fetches without another output write
or receipt acknowledgement. Writer invalidation also clears conditional reuse;
ordinary CLI attachment activation remains unfinished.
Snapshots also retain decoded style rows with finite span/cell bounds and
original overlay order. Broker and client validate row alignment using the
shared rendition interpretation; unknown style metadata is not forwarded.
Snapshots retain decoded cursor and output-mode facts as well, with viewport
bounds checked by broker and client. Transporting those facts does not apply
host terminal modes or establish complete renderer or receipt support.
Outbound output applies the existing attach blink-phase calculation from a
retained client-local epoch. Replacement snapshots preserve the epoch and server
mode evidence; remote metadata cannot set local phase. Idle cursor repaint
scheduling and physical-terminal qualification remain separate.
An explicit internal cursor-repaint API now refreshes receipt-settled output
when effective local visibility changes, using the writer's shared phase rule.
It preserves server rows, revision and the last painted health decoration,
sends no IPC or receipt ACK, and skips unchanged phases. Replacement or writer
invalidation clears eligibility; production scheduling acceptance remains unfinished.
The internal foreground now checks eligible visible blinking cursors after event
settlement and repaints changed phase without server capture or receipt ACK.
Replacement output takes precedence. Exact wake latency and physical-terminal
blink conformance remain unqualified.
Snapshots retain optional Iroh status-slot coordinates and renditions with bounds
checked against delivered rows and viewport cells. Missing/null metadata clears
the retained slot. This metadata-only boundary does not measure connection health
or compose a client-local status pill.
Snapshots also carry bounded receipt IDs without acknowledging them on delivery.
An explicit post-output API binds the last delivered IDs, exact frontend/session
and original mutation key. It must be called only after the renderer commits
output; false settlement stays false and uncertain exchanges are not replayed.
The internal retained-snapshot presenter uses the existing terminal writer and
acknowledges only after the complete frame commits with matching receipt IDs.
For snapshots with an Iroh status slot, it first obtains exact-session health
evidence and composes the shared pill into a copy of the retained frame. Server
rows/styles and receipt identity remain unchanged. Invalid health or known
disconnection prevents output/ACK; incomplete output never acknowledges. This
does not qualify health-driven idle repaint or ordinary CLI attachment.
An explicit internal status-repaint API now refreshes previously painted,
receipt-settled output when coarse health changes. It preserves server rows,
styles and revision, sends no receipt ACK or view request, and skips writes for
unchanged samples. Replacement or writer invalidation clears eligibility.
Production scheduling acceptance and ordinary CLI attachment remain unfinished.
The internal foreground now schedules eligible settled status refreshes after
event settlement on a finite local cadence. Changed health repaints without a
snapshot request or another ACK. Exact wake latency and ordinary CLI attachment
remain unqualified; pending exchanges are never abandoned for a repaint.
Partial or failed output never arms presentation; terminal restoration and
started output tails remain caller-owned. Unix-fd fixtures qualify byte commitment,
not visibility in a physical terminal or complete interactive attachment support.
An internal request-driven foreground composition now handles EOF/cancellation,
bounded primary input and explicit presentation restoration. It preserves
operation identity without replay; abandoning its entire future still requires
the caller's terminal guard. This polling path is not ordinary CLI activation or
qualification of pushed events, production render cadence, clipboard or X11.
A separate internal item-aware foreground now accepts an explicit client-local
clipboard adapter for negotiated primary sessions. Complete validated effects
enter an owned latest-value worker; partial transfers never reach the adapter.
Input-first waits settle the original item exchange before reuse, while
event-first waits preserve unread input. Cancellation retires queued async work
before restoring presentation; already-started backend work may continue.
Queue acceptance is not delivery confirmation or ordinary CLI qualification.
With version-one events negotiated, the internal foreground settles its exact
event poll before reusing the session when input arrives first. Event-first waits
preserve unread input, and idle replies do not redraw unconditionally. This still
does not qualify production render cadence, animations or ordinary CLI attachment.
Identified ordinary events already represented by an exact committed snapshot
cutoff skip redundant capture. Received metadata or unsettled receipts do not
establish coverage; unknown/new IDs and immediate/invalidation actions remain live.
Observer input requests a fresh view after event settlement without forwarding
bytes or issuing a primary mutation. This view-only interaction bypasses ordinary
pacing; EOF still retires the retained frontend and restores presentation.
Internal ordinary redraws now use the shared server-rate cadence after committed
output, retaining one pending latest-state fetch across idle replies. Immediate
redraws, input and resize bypass that pacing. Animation and full production
scheduling acceptance remain separate from this ordinary-rate boundary.
Internal animation refresh now uses the shared deadline after committed output,
requesting a fresh snapshot when due even after idle event replies and while
ordinary pacing is closed. Event exchanges still settle before reuse; exact wake
latency and complete production animation acceptance remain unqualified.
Primary resize admits the new geometry before snapshot capture, reusing an input
step when available or sending an empty step otherwise. View requests alone do
not establish authoritative primary resize, and no input is replayed.
Entry is cancellable and deadline-bound; session ownership retires before cleanup.
The concrete writer retains reset responsibility before writing entry bytes, so
cancelled or failed entry still attempts restoration. Reset delivery remains
bounded and best-effort on an unavailable or backpressured output endpoint.
An internal caller-owned version-one event reader now validates the exact
preface, bounded framing and negotiated codecs without spawning workers. It
retains buffered bytes across cancelled reads and rejects reuse after malformed
or truncated framing. Codec and QUIC fixtures qualify decoding and stream-scoped
setup, not broker event negotiation, frontend forwarding or ordinary CLI support.
An explicitly gated version-two reader now assembles bounded clipboard effects
through the shared transfer owner. Its caller must first validate primary role
and clipboard capability; item-aware reads preserve codec history and cancelled
framing, and partial clipboard content expires while the stream remains live.
Malformed effects discard partial content without becoming transport EOF. This
component does not write a host clipboard or activate session negotiation,
frontend clipboard delivery or ordinary attachment routing.
Internal clipboard transfer framing now retains exact frontend/session/occurrence
ownership on every record. An effect is bounded to 8 MiB, encoded lazily in
256 KiB chunks within 1 MiB frames, and exposed only after ordered UTF-8 commit.
Malformed records clear partial content and poison the exchange with content-free
errors. This component performs no IPC or host clipboard write by itself.
An explicit internal item-delivery request now consumes at most one admitted
clipboard-session item under a finite wait. It binds the exact frontend, delivers
clipboard content through bounded transfer records with nonreused occurrences,
and retires ownership on uncertain delivery rather than replaying the effect.
Ordinary supervision and host clipboard writes remain unfinished.
The consumed frontend item API now validates exact handle/session ownership and
complete bounded UTF-8 transfers before exposing content. It retains occurrence
watermarks across polls and uses one total deadline for all frames. Interrupted
or replayed transfers retire the stream without exposing partial content or
replaying effects; the API does not apply host clipboard policy.
An internal requested event exchange now forwards bounded coalesced redraw facts
and optional event identity across exact-session IPC, without raw event payloads
or unsolicited reply interleaving. Idle waits retain the reader; terminal errors
consume ownership without replay. Real-host sibling fixtures qualify this exchange,
not full event-driven foreground scheduling or ordinary CLI attachment.
An independent internal primary-input exchange carries bounded bytes with the
original mutation key and validates exact frontend/session acknowledgement.
Reported byte acceptance is runtime evidence, not proof of process or model
delivery. Errors consume ownership without automatic replay; full rendering,
events, X11 and ordinary CLI migration remain unfinished. Receipt forwarding is
qualified separately from physical-terminal output commitment.
Input settlement retains runtime refresh and full-redraw flags. Full redraw
implies refresh and discards committed-view reuse and writer state before the
next capture. Missing flags retain the existing no-refresh default; malformed
explicit flags reject rather than becoming invented rendering evidence.
An internal listener supervisor drives finitely many independent setup/display
pipelines. Stalled peers do not serialize siblings; cancellation disposes owned
pipelines without replay. The caller still disposes the listener and completes
endpoint shutdown. Startup election and multi-process CLI activation remain unfinished.
The hidden internal `remote outbound-serve` foreground entry composes these
owners and respects the outbound veto before identity creation. Normal return
removes publication before completing endpoint shutdown. Starting it alone
does not create a remote session or change ordinary attach/new routing; it is
not a supported replacement for full terminal attachment.
Cancellation retains the shutdown owner while profile workers retire. Outbound
profile-lock contention uses bounded nonblocking acquisition, including when a
holder never releases its lock. Retirement deadline exhaustion fails closed.
This does not guarantee process exit during arbitrary stalled filesystem I/O;
Tokio runtime destruction can still wait for genuinely stuck blocking syscalls.
An internal startup-election guard now coordinates launcher ownership at the
canonical configuration root, not a frontend runtime directory. It revalidates
private root/lock objects without replacing them. The guard alone launches no
process and does not replace the endpoint's exclusive identity lock; automatic
startup and ordinary CLI consumer migration remain unfinished.
The internal readiness connector authenticates the Unix peer and negotiates a
bounded hello while retaining the exact local stream. Discovery checks private
root/socket identity without creating or replacing state; socket existence alone
does not establish readiness or remote session authority.
Internal startup composition now joins election and readiness under one deadline,
invoking at most one caller-supplied launcher for missing/refused discovery. It
reprobes after election and retains the guard until readiness. Protocol and
permission failures are not replacement signals. Production launcher selection
and ordinary CLI consumer migration remain unfinished.
An internal explicit launcher accepts an absolute executable and HOME matching
the elected root, uses fixed argv and a cleared environment, and validates private
diagnostics through the held directory. It returns an exact child handle for
observation and reaping; dropping it does not kill a broker shared by siblings.
This is not yet automatic startup or ordinary CLI activation.
Owned startup now composes that launcher with election/readiness, preserving any
spawned child in caller state even on failure or cancellation. An explicit
fresh-binary fixture qualifies actual broker readiness reuse and SIGTERM/reaping
under the isolated environment; this is not full remote CLI attachment acceptance.
An internal CLI selector now supplies the currently running executable to that
owned composition. It retains outbound veto and caller-owned child evidence,
and reuses ready owners without spawning. Eligible first attachments now compose
that selector with paired-profile setup. Explicit process fixtures qualify original
keys, distinct sessions and sibling survival against a synthetic pinned peer;
combined real-host multiprocess and complete remote CLI acceptance remain separate.
An internal client session API consumes readiness once, sends credential-free
setup, and pins session/client/lease identities from the initial line snapshot.
Subsequent snapshots retain the same settlement and stream buffers. Failed
exchanges dispose ownership without setup replay. This remains a line-snapshot
component, not a full terminal renderer or supported ordinary CLI attachment.
Force-kill is distinct from detach and lease administration: it must be
granted when issuing a primary invitation and revokes the selected lease before
terminating its runtime.
Protected profiles report scope `host` or `legacy_session`; old profiles
without scope metadata remain legacy and are not granted host authority. Lease
release, lease revocation, runtime kill, and client-trust revocation remain
distinct.

Remote `mez new` requires a host-scoped profile or invitation. A legacy target
(including an invitation without scope metadata) fails before X11 preparation,
endpoint-key acquisition, pairing or connection setup; it never silently attaches
to the existing session. Use ordinary `attach` for a legacy session target.

Interactive remote attach requires a terminal. A `primary` profile may attach
as primary or observer; an `observer` profile cannot attach as primary.
Compatible clients negotiate supported features, but authentication and
authorization failures are not retried as compatibility fallbacks. A setup
timeout or failed event connection ends the attachment visibly; reattach
explicitly rather than expecting automatic recovery.

Observer resizing changes only that observer's presentation, not shared pane
sizes or another client's terminal. Ordinary redraws follow the server's
`terminal.render_rate_limit_fps` when supported; input and resize remain
responsive. Older peers may not support this redraw limit.

If the connection fails after terminal input may have been sent, Mez reports
that the outcome is unknown. It does not reconnect or replay the input.
Reattach explicitly and check the pane before repeating a command.

For a negotiated primary, completed copy-mode and mouse text selections update
the server session's internal paste buffer and route the copied text to the
attaching machine when client clipboard support is confirmed. Observers cannot
write that clipboard. While a client clipboard route is active, server-host
clipboard commands are suppressed so the copy is delivered through the
client's configured adapter rather than duplicated on the server host; without
a negotiated route, copies retain the best-effort server-host clipboard write.
The client selects its own
`terminal.clipboard_copy_command`; the server cannot provide or override that
command. Writes are best-effort, limited to 8 MiB, and supported through the
same Linux (`wl-copy`, `xclip`, or `xsel`) and macOS (`pbcopy`) adapters used by
local Mez. WSL clients first bridge UTF-8 text to the Windows host clipboard
with Windows PowerShell `Set-Clipboard`, then retain the Linux helper fallbacks.
Headless or unsupported clients continue normally when no clipboard provider
succeeds. Clipboard reads and remote paste are not included.

Create invitation files without exposing the token through shell arguments or
world-readable output. `--output PATH` securely creates a new mode-`0600` file,
refuses to replace an existing path or symlink, and prints only the created
path. Incompatible invitation files are rejected before dialing. Omitting
`--expires` uses `transport.iroh.invitation_ttl_seconds`, including on the
persistent host. An explicit override must be from 30 through 86,400 seconds. For
example:

```console
mez remote invite --role primary --allow-create --allow-kill --output mez-invite.json
mez remote invitation inspect mez-invite.json
mez remote pair --invite-file mez-invite.json --name home-mez
mez --iroh-profile home-mez attach
mez --iroh-profile home-mez list
mez --iroh-profile home-mez kill SESSION_TARGET --force
```

`remote pair` redeems and saves the profile without entering a terminal session.
It uses host-only initialization, which cannot create, select, or attach a
session, and prints the exact reconnect command. Replace `SESSION_TARGET` with
a lease ID, session ID, or exact name returned by `list`. `remote profile list`
and `show` expose only aliases, role ceilings, abbreviated server fingerprints,
and route counts. `rename` changes only the local alias. `remove` deletes only
the local reconnect profile and explicitly does not revoke server trust.
`check` uses the same host-only initialization and reports a secret-free result.
Use local Unix `remote revoke` on the server to revoke a device.

Connection failures identify the setup stage and configured deadline. A setup
timeout also reports pinned direct and relay route counts and states that
Mezzanine authentication was not attempted, so users can distinguish network
reachability from trust rejection without exposing addresses or credentials.

`mez version` prints version information. `mez help` and `mez <command> --help`
show the generated command contract. Human-readable output is the default;
scripts should request `--json` and handle errors explicitly.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Lifecycle, detach, and recovery](../operations/lifecycle-detach-and-recovery.md)
- [Configuration overview](../configuration/overview.md)
- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
- [X11 forwarding workflow](../using-mezzanine/workflows.md#forward-x11-applications-from-a-remote-session)

## Next step

Use [Key bindings](key-bindings.md) for in-session interactive controls.
