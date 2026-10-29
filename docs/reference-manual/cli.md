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

#### Offline installed-native support matrix

The dedicated installed-source acceptance fixture runs the actual CLI in cleared
environments with temporary HOME/default roots: absent-root dry-run, install,
repeat, recognized missing-file repair, and read-only installed preview/check.
Authored settings/statusline/exporter bytes and sibling callbacks survive repair;
shared Codex/Cursor authored members survive repeat and preview. Whole-file
absence repair cannot preserve callbacks already deleted with that file, so that
case is qualified before adding shared authored members.

| Harness / ordinary command | Offline installed-source evidence | Accounting boundary |
| --- | --- | --- |
| Claude / `claude` | Child discovers installed skill/plugin/hooks metadata; SDK-shaped loader shim executes the installed module, real native helper ingress and local timer cancellation | Synthetic current-request final outcomes commit once on replay; missing usage stays unknown and reports a fixed current-epoch coverage gap after prior expense. Not an actual Claude vendor-loader or paid-request check. Native retirement after creator exit is separate from local SessionEnd. |
| Codex / `codex` | Child reads installed hooks.json and executes exact commands; native enrollment, lifecycle and exact SessionEnd retirement | Revision7 synchronous model hooks report checked per-request usage with original native readiness/project origin, separate parent/child custody and durable replay. Native producer replacement under a retained pane root is qualified separately. All known app-server modes require unavailable requesting-client association; this is not default-daemon support. |
| Copilot / `copilot` | Child discovers installed hooks/mezzanine.json exec/args; native lifecycle and exact SessionEnd retirement | `unavailable-safe-activation`: no safe ordinary persistent OTel activation/native correlation is supplied or enabled by this fixture. Existing exporters are preserved. |
| Cursor / `agent` | Child reads installed hooks.json and executes exact ordinary CLI hook commands; native lifecycle and exact SessionEnd retirement | `unavailable-counter-schema`: exact CLI counter/cache/multiplicity/native-origin evidence remains unqualified. No forced print mode or IDE-counter substitution. |
| OpenCode / `opencode` | Separate installer, TUI route/cache and explicit Bun-native fixture; excluded from this four-creator run | The installed client-local TUI observer selects only its own cached root session. Ordinary usage observations are deliberately dropped until durable source continuity is implemented. The legacy server entry remains descriptor-gated and inert under ordinary launch. |
| Pi / `pi` | Separate Pi acceptance owner and fixtures | Not qualified by the four-harness matrix. |

Public contract/source review: 2026-10-28. These references describe available
extension surfaces; they do not attest that a vendor loaded Mezzanine's bytes.

| Harness | Primary reference | Local evidence and remaining scope |
| --- | --- | --- |
| Claude | [Mods and policy](https://code.claude.com/docs/en/plugins/mods/overview) | Installed module/native matrix and separate request/child controls; actual SDK loading remains unverified. |
| Codex | [Inspected hook source](https://github.com/openai/codex/tree/1cc3cf8060c5f1dbd4e3a4d75e1a77b523e0c1ad/codex-rs/hooks) | `codex_` native/projector/installed controls cover the supported direct-source subset; positive shared-daemon requesting-client association is unavailable. The source revision is evidence, not a required runtime version. |
| Copilot | [Hook locations and events](https://docs.github.com/en/copilot/reference/hooks-reference) | Installed classic callback/native matrix; OTel activation, native request correlation and durable ordinary accounting remain separate work. |
| Cursor | [Hook schema](https://cursor.com/docs/hooks) | Installed CLI callback/native matrix; exact CLI usage/counter multiplicity and broader client continuity remain unqualified. IDE samples do not qualify CLI billing. |
| OpenCode | [Plugin discovery](https://opencode.ai/docs/plugins/) and [ordinary TUI](https://opencode.ai/docs/tui/) | `scripts/test-opencode-tui.mjs` checks client-local selection, waits and disposal. `external_enrollment_installed_opencode_tui_uses_client_local_association` is a separate explicit native fixture requiring an existing Bun host; it was not run in this review. Actual vendor loading and ordinary durable usage remain unverified/unavailable respectively. |
| Pi | [Extension source](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md) | Separate lifecycle/accounting and assembled acceptance scope; this matrix does not certify it. |

All six have compiled installation recipes. Installation, actual native
enrollment, known usage, and unverified vendor behavior are separate results.
The four-creator fixture below was rerun on Linux; it does not include OpenCode
or Pi and does not certify macOS, live SDK loading or provider spending.
The separate installed persistent-client test also passed with Node for all six
canonical harness labels. That establishes native sender/transport behavior for
those labels, not an actual OpenCode Bun TUI API or another vendor SDK loader.

Two repeated isolated runs hold four independent creators on two actual pane
roots, alongside an ordinary idle native Harness. They exercise pinned titles,
`:agent-status` without creating a native conversation, native `/status`, and
`:list-agents` focus with no external interrupt/pause/close authority. Real Unix
current-writer/parent/root proof, bounded metadata-only helper requests and storage
redaction are checked; no parent-supplied completion, capability, PID, helper
activation or module entry path supplies admission. The fake vendors emit their
own source-shaped callbacks. These are Linux offline fixture results, not actual
vendor security-policy/loading, macOS, daemon replacement/restart, shared-server
or full billing/release certification. Bootstrap still reports runtime verification
`not-performed`; users do not need a universal certification gate to install a
best-effort supported subset.

For the explicit fixture, build `mez` first, set `MEZ_TEST_NODE_BINARY` to an
existing absolute offline Node executable, then run
`timeout 1800 cargo test --quiet -p mezzanine --lib external_installed_matrix_supported_subset_two_panes_four_creators -- --ignored`.
The fixture does not install a vendor or contact a provider. Default tests leave
this prerequisite-dependent native qualification ignored.

#### Installation and maintenance

`mez bootstrap <harness> [--vendor-version VERSION] [--root ABSOLUTE_ROOT]`
accepts `claude`, `codex`, `copilot`, `opencode`, `cursor`, and `pi` as adapter
candidates. All six now have compiled best-effort deployment recipes; this does
not mean all six have active lifecycle/accounting. Bare bootstrap installs/reconciles; `--dry-run`
previews without writes, and `--check` observes owned state. `--uninstall` and
`--recover` are separate maintenance intents, both supporting `--dry-run`.
`--check` conflicts with maintenance/dry-run; uninstall and recovery conflict.
The old `--apply` and `--plan` flags are rejected, not aliases: scripts should
remove `--apply` for installation and replace `--plan` with `--dry-run` for preview.
The command is daemon-free and does not perform socket
cleanup, vendor executable discovery, credential installation or hook-trust bypass.
Copilot revision7 installs `hooks/mezzanine.json` under `COPILOT_HOME` or
`~/.copilot`, using the documented version1 CLI hook format and direct `exec`/`args`
for sessionStart/userPromptSubmitted/preToolUse/postToolUse/notification/agentStop/
errorOccurred/sessionEnd. It also installs a bundled-SDK observer extension in
`extensions/mezzanine/`. It upgrades exact receipt-owned revision1/2/3/4/5/6 artifacts,
pins the installing Mez executable and
preserves authored sibling hooks and settings values, disableAllHooks, sandbox
grants, policy hooks, credentials or exporters. Receipt-proven edits to that owned
generated document are archived before repair; foreign collisions reject.
Where Copilot already loads extensions, the entry joins the existing session
without adding tools, hooks, permissions or log output. A direct Unix request
attaches to the original classic-hook enrollment only after native writer,
creator and pane-root verification. It cannot enroll a missing run or refresh
health, presentation, leases or accounting. Bootstrap defaults only a missing supported
`settings.json` `enabledFeatureFlags.EXTENSIONS` boolean totrue. It preserves
JSONC comments, other flags and authored true/false values without adopting them;
it never enables the broad experimental setting. Explicit disabled extension
mode/Mezzanine ID or experiments are preserved. Malformed unowned settings are
left byte-exact with an unavailable-settings diagnostic. Exact receipt ownership
removes only an intact owned flag on uninstall, retaining parents and siblings;
edited owned values refuse overwrite. Preview/check stays nonmutating. Extension attachment is an integration
foundation. A separate exact-writer pulse loop now supports finite idle health
where extensions already load. Startup attachment now waits for the original
classic enrollment: at most32 serialized attempts start within15seconds,500ms
apart after failure, with a3second bound per exchange. Shutdown fences late
attachment; first success ends retries. This tolerates initial enrollment or
transport unavailability without creating a run or asserting health from retries.
Outside Mez, missing/invalid ordinary routing leaves the globally installed entry
neutral: it imports no SDK, joins no session and starts no retry timer. Configured
routing still requires actual native peer/source verification; it is no authority.
The owned entry now observes live `assistant.usage` before awaiting attachment,
including subagent metadata, and hashes the full accounting identity. It retains
at most64 immutable model/counter records and sends only after an actual positive
pulse ACK, with at most3 attempts per record. Missing metadata, queue pressure
and unavailable delivery use fixed bounded coverage-gap reasons. Unknown subsets
stay unknown; cache counters are included in input, and billing multipliers are
never currency. The canonical ledger deduplicates compatible terminal replays.
Actual vendor early delivery and loading remain unqualified. Original
request time/project remains unknown; terminal observation is not a provider
dispatch barrier. Activation reports describe the captured default/policy plan,
not proof that Copilot loaded the observer.

The fixed installing-binary peer checker verifies the connected daemon's native
Unix UID before any metadata is written. It borrows the descriptor read-only;
the extension sends the frame itself. No launcher descriptor is required.
Run `mez bootstrap copilot`, then ordinary `copilot` in a Mez pane—no wrapper,
fd3, launch token or separate accounting-enable command. The fixed helper consumes
at most64KiB/250ms before HOME/config/CPU discovery, returning only `{}`. Classic
documented sessionId/timestamp are projected into native-qualified direct-creator
admission and original-session lifecycle observations. Prompt/tool/error text,
transcript paths, credentials and counters are discarded. Permission/elicitation
notifications report waiting; tool/prompt activity clears it; Stop reports ready,
not success or completion. Already active Stop continuation is inert. Stale/tied
conflicting timestamps reject; /clear old sessionEnd cannot retire a different
replacement session. Detached callbacks work only while their genuine creator
relationship remains provable. A preexisting shared daemon outside the pane is
not associated by inherited environment or session labels. Without current
classic activity or concrete extension pulses, idle silence loses
observer health and may expire the lease; it is not continuous renewal proof.
Bootstrap reports lifecycle `unverified`, usage `unavailable-safe-activation`.
OTel is not installed/enabled: official command-reference monitoring supports
process environment, VS Code, or enterprise managed settings, not an ordinary
user hook setting. Bootstrap does not alter parent shell environment, enterprise
policy, existing exporters or content capture. No unqualified receiver/source is
accepted as billing evidence. Supported future request usage must use chat spans
without also summing invoke_agent aggregates; github.copilot.cost is a multiplier,
and nano_aiu is AI units, neither currency. Vendor loading/trust/disable/sandbox
policy still applies. The extension pulse uses its own native process/counter
clock, separate from classic timestamps and status sequences. Duplicate pulses
are time-inert; a new connection must prove the same native writer. Writer death
invalidates extension proof even while the CLI survives. Pulses carry no expense
or state, and daemon maintenance owns lease renewal. Accounting remains pending.
Sources: [classic hooks](https://docs.github.com/en/copilot/reference/hooks-reference)
and [monitoring](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-command-reference#opentelemetry-monitoring).
Cursor revision2 owns only exact sessionStart/beforeSubmitPrompt/postToolUse/
preCompact/afterAgentResponse/stop/sessionEnd members of shared
`hooks.json` under the selected CLI configuration root. A new document receives
version1; existing unsupported versions reject without rewriting. Authored
members/order/metadata and cli-config.json remain untouched, exact receipt-owned
duplicates collapse and missing owned state repairs. Uninstall retains the shared
document and authored entries. Exact receipt-owned revision1 artifacts upgrade
automatically. Run `mez bootstrap cursor`, then ordinary `agent` in a Mez pane—no
Mez wrapper, fd3, launcher token or forced print mode. The fixed helper consumes
at most64KiB/250ms, projects only inert CLI session/generation enums/IDs through
native-qualified direct-creator admission, and returns `{}` on all telemetry loss.
It returns no env/context/follow-up/permission fields. SessionStart must carry
matching session_id/conversation_id; child fields and ambiguous selectors drop.
Distinct interactive prompt generations fence old Stops. Completed means ready,
not proof of success; aborted/error mean interrupted/failed. Compact/tool/response
activity never bills context tokens or reopens a terminal generation. Exact old
end cannot retire a different session or rotated observer. Idle silence and a
shared preexisting daemon lack persistent observer/client association proof.
Bootstrap reports lifecycle `unverified`, usage `unavailable-counter-schema` in
plain and JSON. Vendor staff confirms interactive afterAgentResponse counts and
stop status/loop_count, but current reference documents only response text, not
the exact ordinary CLI counter shape/inclusive subsets/stable report/request
origin. Sanitized IDE evidence and a public observer list flat input_tokens,
output_tokens, cache_read_tokens and cache_write_tokens; they do not prove CLI
parity, inclusive-cache arithmetic, request multiplicity or provider identity.
The same IDE sample repeats totals on response and Stop, so summing both is unsafe.
The public CLI fixture corroborates session_id/conversation_id equality and the
non-interactive generation fallback, but contains no response counters. Missing input
is unavailable, not zero. The known non-interactive generation_id==conversation_id
fallback is not turned into a synthetic turn; older non-interactive releases may
omit prompt/response/stop entirely. No transcript/terminal scrape or mandatory
headless mode substitutes for an ordinary usage source. StatusLine exists but
its counter schema is not documented here; bootstrap leaves user footer/title
pins untouched. Enterprise OTel is server-side admin/public-HTTPS/auth configuration,
not safe local bootstrap activation. Retry accounting when an exact ordinary CLI
counter/request identity contract and native original-expense mapping are available.
This lifecycle phase does not close continuous idle/shared-daemon or usage work.
Loading under custom roots, policy, sandbox and relocation remains best-effort;
actual vendor/macOS acceptance is not certified by isolated fixtures.
Sources: [CLI changelog](https://cursor.com/docs/cli/changelog.md),
[hook schema](https://cursor.com/docs/hooks.md), and
[vendor CLI explanation](https://forum.cursor.com/t/cursor-cli-omits-beforesubmitprompt-afteragentresponse-and-stop-hooks-loses-token-usage-and-emits-inconsistent-generation-id-values/169059/9)
(2026.08.11 evidence, not an installation/version gate).
Sanitized [CLI fixture](https://forum.cursor.com/uploads/short-url/iHJHbzkLkvSnQdUWTOQ6xW9dUyv.txt)
and [IDE comparison](https://forum.cursor.com/uploads/short-url/eTwk1a31hJzz1y07lVGEsOdSpi3.txt)
are schema evidence only, not captured/live execution prerequisites. No fixture
prompt/response, path, email or transcript data is installed or retained.
The fixed internal `mez codex-hook` callback mode runs before HOME/config/CPU
startup. Normalized main SessionStart startup/resume/clear/fork can make one
restricted Unix declaration with standard MEZ routing and native creator proof,
without fd3 or a launcher credential. Every outcome returns neutral JSON; missing
route/input or daemon outage cannot expose errors or initialize a client.
Prompt/Stop/Interrupt update only the original fixed first epoch through native
callback proof and daemon sequencing. SessionEnd retires only the native-qualified
original first-epoch session, not its creator process or a successor. Changed
native callbacks provide finite 30s proof for existing daemon 60s lease maintenance;
replays/creator survival do not restore lost proof. Continuous idle health and
usage remain unqualified. Only exact helper argv uses
this boundary, not ordinary CLI commands or option-bearing variants.
`--root` is an optional override, not a prerequisite: compiled adapters select
their documented user root from vendor environment/defaults. Those selected roots
may be absent: inspection stays write-free and accepted installation creates only
the captured missing suffix. Root-policy availability does not imply a compiled
adapter exists. Missing adapters fail mutating intents before root discovery;
check/dry-run return unavailable diagnostics without mutation.

The Gemini external harness is retired, with no replacement. Every Gemini
bootstrap intent is rejected before root access. New canonical `gemini` launch
and external usage admission are also rejected. Historical expense and replay
guards remain intact; independent Google/Gemini provider and model usage under
native or other harnesses is unaffected. No vendor files or user hooks are removed.

**External vendors are invoked normally, not through Mez wrappers.** The CLI
contains no vendor-launch routes, aliases, hidden launchers or deprecated
forwarding. Bootstrap and observation-only internal helpers do not launch vendors.
Pi/OpenCode fixed persistent clients accept only native installing executable
evidence that is absolute UTF-8, at most 4096 bytes and free of control characters.
Unsupported evidence emits the existing inert undefined helper reference, never
PATH, vendor payload or launcher fallback. Literal punctuation and Unicode are
preserved as JSON data. This is bounded deployment evidence, not safe adoption of
unknown relocated receipt references or proof of vendor activation.

Bare install and write-free dry-run are available. Recognized compiled predecessors
can refresh through the shared ownership, preservation and interrupted-publication
recovery mechanisms. Ordinary-command enrollment coverage and durable source
continuity remain separate vendor/runtime work, not proof supplied by installation.

**Unsupported legacy receipts:** an unrecognized compiled revision or old helper
reference is a non-destructive conflict, not permission to overwrite the installation.
Keep the existing vendor files and private receipts/journals intact. `--check` and
`--dry-run` may report the same conflict; they do not repair an unknown identity.
Use a trusted installer that recognizes the original receipt for its supported
maintenance operation, or review the legacy installation for manual migration.
Preserve authored hooks/settings and remove only entries whose ownership has been
independently established. Do not edit receipt hashes/helper paths, discard journals,
or delete an entire shared configuration merely to make the current installer accept
it. Rebootstrap from a different executable pathname does not authenticate the old
helper, and automatic migration of such unknown references remains unsupported.
Installation is not proof that
callbacks are loaded, enrolled or accounted. Legacy descriptor-gated server
entries remain inert under ordinary invocation; the installed client-local
paths have the separate capabilities and limits listed above. Vendor
trust review and disabled policy remain intact. No live/provider/platform
conformance is implied by component or historical launcher tests.

The best-effort Codex component projects bounded main-session lifecycle
identifiers from documented hook schemas without requiring an exact version.
An exact parent-owned session filter ignores other sessions; duplicate top-level
identity/event keys and objects over 256 fields reject without exposing payloads.
Stop is reported as ready/turn-boundary, not proof of success: other hooks may
cause continuation. Interrupt remains an interrupted-turn observation, while
SessionEnd does not attest process death. It discards prompt/transcript/tool
content and child-context events, and does not infer token usage. This pure
component does not deliver launch capabilities, renew idle leases, install hooks,
or certify a live Codex process. Its release reference is
`openai/codex` tag `rust-v0.160.0`, `codex-rs/hooks/schema/generated`.
That reference is evidence, not a compatibility gate. The fixed internal helper
normalizes at most 64 KiB/250 ms of stdin and remains neutral on unavailable
telemetry; it never starts Codex, initializes a primary or bypasses vendor trust.
Codex bootstrap revision5 adds observational `PreToolUse`, `PermissionRequest`
and `PostToolUse` callbacks. They retain only the exact main session/turn identity:
permission waits can show `approval-wait` and tool activity can show `running`.
They cannot start a turn, revive an interrupted/old turn, update the parent from
a child callback, approve a tool or forward tool content. Existing revision4
members upgrade through the shared-hook reconciler; authored hooks and normal
vendor trust review remain intact. This lifecycle coverage does not supply token
accounting or change existing exporters.
After idle proof and lease expiry, the original Codex source becomes invisible
and its observational presence goes offline, while bounded original request and
replay custody remains under the same live native creator/root. Maintenance and
duplicate callbacks do not renew it. A changed lifecycle fact with fresh native
worker/actor proof restores the same identity and a finite lease; SessionStart
retries and model counters alone do not. A long-running request's original post
can still commit usage while dormant. Explicit end, root loss and observer
epoch replacement stay fenced; this does not establish continuous observer health.
A reused public conversation label cannot choose another native creator: native
parent evidence narrows the candidate before full worker/actor proof.
Known shared app-server command hooks cannot identify their requesting client
from a launching pane's inherited MEZ, ancestry or cwd. Known app-server modes,
including stdio with potentially persisted remote control, are refused with
`unavailable-codex-native-client-session-association`; unavailable native argument
inspection reports `unavailable-codex-native-process-topology`. Arguments remain
private and supply no authorization. Bootstrap reports the shared-server gap
separately from unverified runtime support. Vendor launch options, shims, daemon
policy and exporter configuration are unchanged; no `--no-daemon` requirement is
introduced. Ordinary client-side session association still needs qualification.
Codex model readiness measures the actual native Pre hook working directory,
which can differ from its creator's directory. Only a trusted mapped project is
attributed; untrusted or unavailable hook cwd stays unattributed. Later Post
callbacks and retries retain the original project, regardless of their cwd.
Payload paths and pane cwd hints cannot select accounting origin.
Revision7 also installs SubagentStart/SubagentStop. Child usage requires a genuine
native start under the exact original parent session; child IDs alone do not
establish association. Main and sibling requests keep separate custody while
sharing the finite request budget. Child facts do not repaint or renew the parent.
SubagentStop may continue, so it does not prove death or discard readiness. Later
Pre callbacks can admit new requests, while old retired turns stay fenced. Parent
end, root loss and observer replacement close the original child association.
Child hook cwd supplies original project attribution; no transcript is read.
The previously inspected lifecycle hooks at upstream `openai/codex` commit
`0c329ad2` have lifecycle IDs/selected model rather than usage counters. Newer
observation-only `PreModelRequest`/`PostModelRequest` hooks also expose per-request
usage. The internal pure projector checks inclusive counters, keeps child scope
explicit, and preserves valid incurred usage after failure or cancellation.
Missing categories remain unknown and explicit zeros stay known. It drops error,
path/content and selected-model fields. These hooks have no request ID/index;
normalization alone does not install them or qualify native request pairing,
original origin, response model/provider or durable accounting. An internal adapter now routes these facts through native helper qualification
and the shared original-origin ledger, with finite custody and replay/overlap
fences. Retained reports retry after storage refusal without changing their
original admission; recovery clears their delivery gap while preserving
independent missing-counter/readiness/capacity gaps. Revision6 installs these two
model hooks explicitly synchronously with a three-second cap. The helper makes
at most two immutable bounded delivery attempts and requires a typed durable
positive-revision ledger acknowledgment for known incurred usage. Revision5 and
older receipts upgrade while preserving authored hooks/config/exporters/trust.
Direct native child continuations and producer replacement beneath a retained
pane root have separate local qualification. Positive default shared-daemon
requesting-client association remains unavailable; a full Mezzanine daemon
restart preserving PTYs is not established by those tests. Separately, OTEL
completion events and response spans expose
input/output/cache-read/cache-write/reasoning counters.
TUI `last_token_usage` can mean active context occupancy, while `total_token_usage`
is accumulated session usage; neither is an independently qualified new request.
No OTEL accounting receiver or metadata-only exporter activation is installed by
this adapter. The inspected completion-log method does not take a response/request
ID or request-frozen origin; OTLP trace/span correlation and native requesting
client association, especially with a shared daemon, still require qualification.
Conversation labels, exporter peers and timestamps cannot select billing owners.
Codex selects one exporter per signal, not an appendable sink list. Replacing an
authored exporter would change its destinations/privacy contract; detailed logs
also include account/email and optional content. A future source must preserve
authored exporters, filter allowlisted metadata, retain unknown versus zero,
qualify response model/category/original expense origin, and provide typed durable
same-report acknowledgments without transcript reads or provider replay.
Same-ID startup retries are not clear/resume/fork transitions. The neutral fixed
hook discards admission replies and retains no exact predecessor; changed boundary
is fenced rather than silently rotating/recharging. Actual source handoff and
durable pending recovery or cumulative attachment remain explicit unfinished work.
The revision-4 hook candidate owns only exact compiled members of five lifecycle
event arrays in shared `hooks.json`. Authored callbacks/matchers/timeouts, unrelated
events, metadata, disabled policy and `config.toml` survive install and uninstall.
Exact receipt-owned duplicates collapse at the first position; unreceipted matches
or edited/missing owned members in an existing document conflict. A genuinely
absent whole file can be recreated only from current compiled members under a
qualified current or frozen historical receipt; missing-file uninstall leaves it
absent. No stale receipt payload becomes new ownership. Recognized revision-1 whole-file receipts
migrate to shared ownership during current reconciliation without deleting authored
additions; current uninstall retains the containing document. Strict-JSON changes
may reformat whitespace, but noops are byte-exact and JSONC comments are not stripped.
Revision-4 members pin the safely quoted absolute installing Mez executable,
not `MEZ_CODEX_HELPER` or a PATH lookup. A missing/nonexecutable helper returns
neutral JSON; restoring the executable at its pinned location restores delivery.
Builtin test/exec/printf with short-circuit operators supports Sh/Bash/Zsh/Fish
without forcing Codex to another shell. Exact revision-3 and revision-2
shared members are retained as immutable history and replaced without deleting
authored callbacks. Rebootstrap at the original executable location is unchanged;
relocation qualification remains unfinished. A receipt containing an unknown
old helper pathname is rejected rather than adopted as ownership authority.
Ordinary-command SessionStart now reaches the native creator admission primitive
through one bounded Unix exchange. Prompt/Stop/Interrupt observations are coarse,
arrival-ordered and original-epoch fenced. Known retired turn IDs cannot repaint
new turns; exact retries are inert, Stop is ready rather than success and later
Stop cannot downgrade interruption. The 4096-turn run-local fence rejects
exhaustion without eviction; unknown new prompts do not prove vendor chronology.
Callbacks do not directly renew leases. A new native-qualified changed fact
provides finite observer proof: recent proof plus live creator/root permits daemon
maintenance to renew its 60s lease, while proof loss remains connection-lost even
if the creator lives. Duplicate/stale/failed/end facts do not refresh proof;
the maximum grace remains 30s proof plus 60s lease, not continuous idle certainty.
Initial admission exposes awaiting-first-proof and usage stays unavailable.
SessionEnd uses exact
ordered retirement, leaves its live creator/expense/unrelated sessions untouched
and fences queued work and same-lifetime resurrection. Same-ID transitions and
restart attachment remain unqualified; continuous idle-source and broader client-session
association remain unfinished. Ownership installation is
not proof of loaded/enrolled hooks. No token expense is inferred from hooks or
ambiguous context/cumulative structured samples.

The best-effort OpenCode component projects settled assistant-message snapshots
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
These are evidence, not required runtime versions. Observed version text does
not alter the stable bound-session/message accounting identity. A transport-free
plugin producer emits only coarse root-session status and allowlisted completed
message counters; content, paths, tool parts and cost estimates are dropped.
Session metadata updates do not imply idle, and permission/question replies
settle only their exact request IDs; overlapping waits remain visible and stale
or duplicate replies cannot clear newer ones. Pending/resolved history is capped
at 256 IDs; exhaustion reports unavailable once and disables producer telemetry
instead of evicting history. Unknown/prototype-named statuses remain inert.
The pure producer opens no channel or client API. The dependency-free owned entry
remains `plugins/mezzanine.js` plus private siblings; settings and other plugins
are preserved. That server entry retains its legacy descriptor gate and stays inert under
ordinary invocation; it is not a wrapper fallback. Revision15 also registers a
separate client-local TUI plugin in the effective `tui.json`/`tui.jsonc` plugin
array, preserving authored fields. Its selected route and root-session cache,
not the server bus or inherited server pane, select lifecycle observations.
The native helper/persistent client still qualify the requesting process and
pane. Unsupported/missing TUI APIs remain inert. This ordinary TUI path
intentionally drops usage observations until durable accounting-source
continuity is implemented; it does not adopt server-wide usage. Existing checked
completed-message ledger replay/conflict protection and strict frame fixtures
remain reusable, but do not prove normal callback connectivity. Coverage remains
finalized assistant messages with complete evidenced counters, excluding tool
parts/auxiliary calls and invoice costs. Vendor pure/disabled behavior is unchanged.
Offline checks: `timeout 120s node --test scripts/test-opencode-*.mjs`.

The Pi coding-agent lifecycle component projects content-free lifecycle facts
for an explicitly bound context session. `agent_end` and `turn_end` are not final
settlement; `agent_before_settle` supplies only a provisional outcome, and
`agent_settled` reports final settlement without inventing a successful outcome.
Extension UI prompts indicate input wait, not automatic approval wait. Shutdown
reasons distinguish quit from reload/new/resume/fork, not prove process death.
This component does not install an extension, deliver credentials, renew leases,
rebind sessions or certify live usage. Sources are the installed
`@earendil-works/pi-coding-agent` `1.0.2` extension declarations and agent-session
producer. Pi work is independent of the retired Gemini harness, not its replacement.

Ordinary `mez bootstrap pi` now also installs finalized assistant request
accounting. Run normal `pi` in the Mez pane; no `mez pi`, fd3, preselected session,
`--apply`, version/root gate or separate accounting enable step is required.
Pi revision 19 recognizes independently frozen revision-18 and all earlier
compiled owned bytes for upgrade/repair/repeat; disabled vendor policy, authored
entries and trust remain unchanged. The revision-19 redisclosure update changes
only the Pi usage producer; shared transport and OpenCode15 owned bytes remain
unchanged, without adding OpenCode usage behavior.

The producer observes genuine `turn_start` readiness, then persisted `turn_end`
assistant entries. `message_end` is not charged: other extensions may still
replace the outcome before persistence. A same-object pairing or preserved
provider/responseId correlation links only the original request ticket. Missing
correlation is unavailable rather than a guessed current turn. Response attribution
uses the actual provider and concrete `responseModel` when available, not selected
model UI. Input is `input + cacheRead + cacheWrite`; reasoning is an output subset
and one-hour cache writes are a write subset. Required missing counters or invalid
numeric/subset data are not zero; absent reasoning stays unknown. Finalized
errors/aborts with counters can record incurred usage, not successful task proof.

Only a typed durable ACK commits reporting. Retry retains the same marker,
persisted-entry digest, normalized payload and once-frozen reporting UTC, and
never retries a provider. Native begin captures actual admission cwd/time; the
asynchronous callback queue does not guarantee earlier callback-time attribution.
The callback queue is bounded at32 and process-owned active-plus-pending metadata
at128, reserved before asynchronous begin. Only durable historical replay evidence
may be pruned. Full pending capacity refuses new telemetry admission with a
truthful readiness gap; it never abandons incurred unknown-delivery obligations.
Exhausted or closed pending slots can saturate this process-local capacity until
process exit, including across reload. This is not a durable expense spool.
The same healthy observer retries frozen finalized reports without another vendor
callback through one unreferenced timer and one queued/running recovery operation.
Initial delivery plus at most three retries share a finite callback/timer budget
(at most eight wire report attempts). Timer offsets are250/1000/4000ms from first
undurable completion with an absolute5000ms monotonic dispatch horizon, rechecked
inside the queue; delayed work retains spacing rather than bursting to catch up.
This is not a five-second completion guarantee. Duplicate callbacks cannot reset
the horizon/budget. Exhaustion retains the sticky gap and original obligation.
Failure, settlement, teardown and stale ownership cancel/fence future recovery;
new runs cannot revive closed slots. A late real ACK can settle its original
obligation without restoring a failed or stale owner. No provider work is retried.
Genuine reload reuses process-owned replay evidence, but successor credentials
cannot inherit old tickets. Unmatched restored/history events are dropped.

Pi revision18 also discloses observed auxiliary usage without guessing its
provider, model or request origin. A compaction/branch-summary callback with a
`usage` property, or tool-result usage on a matched current assistant outcome,
sets `unavailable-pi-usage-response-correlation` in `/status`. A known gap also
remains visible immediately after same-live native recovery, before another Pi
callback creates the recovered lifecycle projection. The producer reads
only property presence, never auxiliary counter values or content; it inspects
at most128 tool-result slots and discloses unqualified coverage for larger
lists. Empty navigation stays inert. Available main assistant usage is still
recorded normally; the gap takes precedence in coverage status. Startup does
not duplicate a current pre-readiness gap already queued by its callback.
Revision19 rediscloses a later genuine auxiliary observation after earlier
diagnostic custody settles, so process-local last-reason metadata cannot hide
receiver-side state loss. Copies already queued/in flight coalesce; failed or
stale observers and historical finalized replays stay inert. It preserves exact
frozen revision18 and earlier layouts. Shared transport, normal failure-retry
budgets and settings remain unchanged; no extra enablement is added.

Known limits: pending/deferred responses, compaction/branch/tool/cache-warming
auxiliary calls without a stable current-request counter contract, and durable
cross-process unsent-report recovery are unsupported. No transcript backfill or
invoice/full-spend claim is made. A genuine new/resume/fork callback may enroll
a distinct, never-retired native session namespace. Returning to the same
physical producer/session after its actual shutdown retired that namespace stays
`unavailable-source-continuity`; a new observer name cannot reopen it. This
protects prior expense identity and does not report resumed-session usage as
covered. Same-session reload is a separate successor handoff that preserves its
qualified source. Fixed `unavailable-pi-usage-*` reasons disclose
missing counters, correlation, readiness, clock or durable ACK through `/status`;
they leave already recorded totals and lifecycle state intact. A later successful
report does not erase a sticky gap within the observer epoch. Native transport
health remains separate. Coverage disclosure/recent retry metadata is not a
durable expense spool or an exact count of dropped reports.

If a genuine new run, final agent settlement, or supported session teardown
arrives with an unfinished request, missing finalized counters are disclosed
before the original ticket is abandoned and before native retirement/detach.
The callbacks remain synchronous and neutral while readiness is pending. Already
completed reports do not gain a false loss, and an unmatched historical outcome
cannot close the current request. This is an unknown coverage gap, not invented
expense, zero-spend proof, or a fabricated task-completion event. Pi revision 11
introduced this ordering. Pi revision 12 and current OpenCode revision 11 also
ship the strict shared client resume parser, with exact pre-resume transport and
all prior artifact layouts independently frozen for upgrade/repair.

Same-live-source reconnect is explicit: the client captures its accepted public
observer witness/generation and original predecessor before awaiting enrollment.
Lost replies retry that immutable tuple, never the newest generation or a bare
initial fallback. Only a typed resume reply (`same-live-source`, next ordinal 1)
with changed handle, same run and checked epoch advance resets presentation
sequence once. Same-handle normal reconnect and repeated already-accepted resume
receipts retain their sequence. A successor carries only public expectations;
old request tickets, markers, report UTC, source witness and project origin stay
on their original client and remain subject to native readiness revalidation.

Selectors alone grant nothing. Native retained control ownership and live Unix
writer/root/creator evidence are required; absent original service anchors or
retired/replaced source cannot be recovered from a database row or label change.
If a resumed reply is lost and its new registry binding is then lost again, the
old selector may remain unavailable; the client does not invent shadow authority.
After an answered warm refusal, the shared client may attempt explicit durable
source recovery using its original public checkpoint and a frozen operation ID.
Retained same-live authority must refuse cold replacement after an unknown
observer reply; a historical checkpoint cannot select a newer observer or
charge the original report again.
The client sends no synthetic startup, outcome or diagnostic baseline on recovery.
The first actual Pi gap may use ordinal 1 only on a native resume-qualified
projection; a lost gap ACK stays failed. Lifecycle/gap delivery failure remains
latched unavailable in this initial scope, with no silent revival of cancelled
or stale owners. This is not full OS-process/pane restart, durable unsent spool,
automatic recovery of every callback, or native-server qualification by mocked
wire tests. Actual installed combined recovery fixtures are separately scoped.

Source evidence: public Pi revision
`7bd01fb92485c06f92ecfff7ccf3197148bc16af` (`agent-session.ts`, extension
`types.ts`/`runner.ts`, agent loop, and Pi-ai usage types) establishes callback
ordering, persisted IDs and subset semantics. It is evidence, not a version gate.
Offline tests: `timeout 120s node scripts/test-pi-persistent.mjs`,
`timeout 120s node scripts/test-pi-usage.mjs`. Explicit installed producer/native
qualification uses `MEZ_TEST_NODE_BINARY=/absolute/node timeout 600s cargo test
-p mezzanine --lib --all-features --quiet
external_enrollment_installed_ordinary_pi_usage_has_bounded_durable_stream -- --ignored`.
This runs synthetic source-shaped callbacks through the real installed default
producer/Unix peer/ledger, not a live vendor or provider.

The assembled bare-bootstrap qualifier is separate from that internal-plan
fixture. First build the product binary, then run:

```sh
timeout 900s cargo build -p mezzanine --bin mez --all-features --quiet
MEZ_TEST_NODE_BINARY=/absolute/already-installed/node timeout 180s cargo test -p mezzanine --lib --all-features --quiet external_pi_bare_bootstrap_ordinary_launch_accounting_and_retirement -- --ignored
```

It invokes actual `mez bootstrap pi --dry-run` and bare `mez bootstrap pi` under
an isolated HOME: default-root discovery, missing-root read-only preview,
install/repeat and private owned-edit archive/repair preserve unrelated settings
and extensions. A normally invoked local `pi`-shaped process independently finds
the installed package entry and emits source-shaped callbacks. No explicit root,
version/apply flag, entry/helper argument, enrollment token, vendor observer fd3,
manual artifact renderer or accounting enablement is supplied. Private marker
gates sequence the synthetic vendor only; the child alone creates callbacks and
reports through genuine native Unix origin/writer proof and the production actor.

| Assembled local boundary | Qualified assertion |
| --- | --- |
| Factory and ordinary startup | Resource-free/duplicate-neutral factory; exactly one native producer enrollment with no controls or client initialization. |
| Lifecycle and presentation | Current input-wait, `/status` and `:list-agents`; pinned user title and retained native conversation coexist without provider work. |
| Finalized usage | Success/error/abort callbacks, actual response model, inclusive cache partitions, fresh frozen reporting UTC, replay deduplication and three typed durable ACKs. |
| Missing counters | A fourth original readiness marker has no linked observation/expense; explicit coverage gap preserves earlier totals, never invents zero expense. |
| Completion and outside Mez | Exact session retirement, a separate typed durable source-end receipt and cleared reservations; outside-Mez callbacks preserve normal output/exit and request no telemetry authority. |
| Privacy | Only allowlisted observational methods and metadata; prompt/answer/tool/error content and raw upstream response identity stay out of transport. |

This is offline assembled qualification, not a real Pi SDK loader/provider run,
multi-pane/disabled-policy certification or spend completeness. Same-live-source
committed-ACK loss with registry reset/reconnect and periodic ordinary idle renewal
remain separate **source-feasible** acceptance/implementation work, not external
environment excuses. Real SDK loading, macOS execution and manual/live-provider
coverage must be recorded independently. Legacy inherited-descriptor fixtures below
are isolated utilities, not required setup for ordinary Pi commands.

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
These integration primitives remain reusable after wrapper removal; they do not
establish ordinary-command enrollment or complete accounting on their own.

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
losing its result does not undo an accepted activation command. The inherited
bridge confirms same-session reload after ordered shutdown. The v2 public parent
separately authorizes new/resume/fork with an immutable fresh owner and capability.

The internal v1 inherited-stream bridge connects content-free observer facts to
coordinator ingress. The launcher supplies a connected same-user Unix stream,
session and epoch; none is selected by a callback payload. Exact JSON lifecycle
frames are limited to 1,024 bytes, with a 250 ms total partial-frame deadline
and no idle-silence timeout. The JavaScript sink bounds buffered writes to
32 KiB and stops accepting telemetry after backpressure or error without
changing vendor decisions. Daemon capabilities never cross this observer link.
The launcher retains session lifetime separately from observer stream closure.
The retained v2 protocol fixture carries bounded child-proposed session
and observer epoch plus the same strict allowlisted lifecycle facts, limited to
2,048 bytes with a 250 ms partial-frame deadline. Those proposals grant no
authority. Future ordinary-process admission must establish ownership separately;
credentials never cross this observation channel.

The injectable Pi extension factory wires the observer to its stream sink only
at session start. Factory loading opens no resources; a matching context session
can open its explicitly supplied channel once, and shutdown forwards an inert
teardown fact before idempotent cleanup. Failures remain neutral without retry or
automatic rebinding. Offline tests exercise this wiring through the installed
Pi 1.0.2 loader/runner; public installation and initial-session launch are
best-effort, while live/platform conformance remains a disclosed validation gap.

The internal observer launch primitive now performs the inherited-descriptor
handoff used by the offline loader fixture. Its caller supplies an absolute
executable and directory, exact arguments, environment and stdio; no ambient
environment or executable discovery is used. Descriptor 3 carries observations
only. The process owner must reap the child, independently of observer disposal
or telemetry failure. This primitive is test-only for offline descriptor
qualification, not a public/hidden launch command. It does not establish normal
enrollment, live/platform conformance or a vendor-launch fallback.

The internal owned observer runner joins strict callback ingress and session
lease delivery without detached tasks or child ownership. Clean stream EOF drains
accepted facts before ending renewal; it does not imply process death or request
deregistration. Explicit quit facts retain their existing exact-session retirement
semantics. Errors and cancellation preserve pending reducer evidence without
automatic reconnect or vendor replay. Production authorization and replacement
session ownership remain separate and require the normal-path enrollment owner.

An internal compiled artifact candidate owns six files under
`extensions/mezzanine/` in an explicit agent-directory root. A package manifest
selects one entry point; private `.mjs` siblings are not independently discovered
as extensions. The entry is registration-only during loading and opens descriptor
3 only at matching session start with explicit observation markers and a socket
check. These markers carry no daemon capability. Temporary-root repeat, conflict
and uninstall checks and an offline Pi 1.0.2 discovery/runner fixture qualify the
artifact ownership. Public bootstrap installs these same compiled files, without
an exact vendor-version gate or a universal compatibility claim.

The common engine owns exact whole files or compiled exact entries in shared JSON.
Edited ownership conflicts rather than overwriting user changes. Repeat is
byte-stable; uninstall removes owned entries rather than restoring stale backups.
Strict-JSON changes may reformat surrounding whitespace. Qualified OpenCode TUI
JSONC uses local token edits retaining comments/unrelated bytes; unsupported formats
and TOML are not silently converted. Publication uses no-follow directory handles, bounded regular
files, a cooperating-installer lock, exact preimages and a private forward-recovery
journal. A partial transaction remains explicit; recovery refuses foreign edits.
Final check plus rename is not atomic CAS against arbitrary external writers.
Planning and `--check` now inspect through held no-follow read-only handles,
without creating or acquiring an installer lock, journal, receipt, artifact
parent or stage. They can inspect a normally readable nonwritable root and do
not contend with a cooperating publisher. Apply/recovery acquire the private
nonwaiting lock and recheck physical root identity, pending journal and every
captured preimage, including unchanged artifacts and receipt. A no-op apply also
rejects changed observations without writing a journal. Missing roots retain a
held closest-existing ancestor and bounded absent suffix; preview reads neither
its root files nor its journal. Accepted install revalidates original ancestor and
first-entry absence, creates only that suffix with no-follow handles, then uses
the normal locked publication boundary. Competing creation/replacement rejects
before artifact publication. Existing ancestor modes/ownership stay untouched.
No-op uninstall stays absent; explicit recovery never creates a root. Creation
is not atomic: an error may leave created directories, with no foreign/nonempty
cleanup sweep. Normal-directory access uses OS permissions independently of
protected private state; archive/edited-file preservation remains unfinished.
Recovery dry-run uses the same journal version/root, compiled-intent
and before/after preimage checks as recovery, but takes no lock and finishes no
effects. JSON `operation` is install/check/uninstall/recover; `dry_run` is explicit,
`changed_paths` describes planned edits and `recovered` requires actual settlement.
`recovery_pending` distinguishes accepted recovery preview from no journal. Check
currently returns success when inspection succeeds, even if changes are needed;
scripts should inspect `changed_paths`, not infer installed/active state from exit0.

Plain bootstrap output now summarizes the result, selected root/source, planned
change/preservation counts, confirmed preservation and pending/completed recovery.
JSON `result` is `installed`, `upgraded`, `repaired`, `uninstalled`, `recovered`, or
`unchanged` only after successful actual work; read-only intents report `preview`
or `checked`, and absent adapters report `unavailable`. `planned_outcome` classifies
the requested reconciliation after original pending intent has been accounted for;
`unchanged` with `recovered:true` therefore means the original journal was settled
but the requested current reconciliation needed no further effects. Receipt-only
migration counts as repair. `planned_preserved_paths` includes requested and pending
recovery requirements, even for reused archives; `preserved_paths` stays empty for
check/dry-run and reports those paths only after successful actual settlement.
No archived bytes are printed. Explicit recovery now returns its original journal
`changed_paths`, including already committed effects, rather than an empty path list.
`runtime_verification:"not-performed"` makes clear installation is not proof of
loading, enrollment, token coverage or vendor compatibility. No vendor/daemon probe
or extra enable command is run for this report. `scope_root` and `root_source` are
destination diagnostics, not inferred project scope/trust or loader discovery.

For compiled adapters, `--root` is now optional. The selector uses an explicit
absolute override first, then the vendor directory variable, then its user default:

| Harness | Directory override | Default root |
| --- | --- | --- |
| Claude | `CLAUDE_CONFIG_DIR` | `~/.claude` |
| Codex | `CODEX_HOME` | `~/.codex` |
| Copilot | `COPILOT_HOME` | `~/.copilot` |
| Cursor CLI | `CURSOR_CONFIG_DIR` | `~/.cursor` |
| Pi | `PI_CODING_AGENT_DIR` | `~/.pi/agent` |
| OpenCode | `OPENCODE_CONFIG_DIR` | `~/.config/opencode` |

OpenCode honors `XDG_CONFIG_HOME/opencode` on supported Unix platforms; Cursor
honors `XDG_CONFIG_HOME/cursor` on Linux/BSD, not macOS. Copilot's old XDG location
is not selected. `OPENCODE_CONFIG` and `OPENCODE_TUI_CONFIG` are file overrides,
not plugin roots. Selection is literal: missing HOME when needed, empty/relative/
control/traversal/oversized selected values fail without fallback or `~` expansion.
Explicit roots need no environment lookup. JSON reports `scope_root` (lossy display
for non-UTF8 Unix spellings) and fixed `root_source`; actual filesystem path bytes
remain exact. No vendor probing or cwd/focus/project ancestor discovery occurs.
The six root policies do not imply six compiled adapters: absent adapter diagnostics
still perform no root discovery or mutation. Missing selected roots now support
read-only preview and installation-only anchored creation. Bare install and
`--dry-run` now use these owners. Missing private whole-file artifacts under a
recognized receipt can be recreated by ordinary install, including upgrades;
already absent private files do not block uninstall. Check/dry-run captures
absence without creating artifact parents, and apply revalidates it before
no-replace publication. Unqualified unreceipted content remains a conflict; only
the complete exact private-bundle adoption described below is an exception.
Receipt-proven edited
Pi `.mjs` helpers, the private `extensions/mezzanine/package.json` loader metadata,
and OpenCode generated JavaScript/module helpers now preserve exact preimages in
private content-addressed archives before replacement/removal. Only that exact
compiled Pi package destination qualifies; shared package metadata, hooks and
config do not inherit that exception. Missing shared
JSON ownership is not treated as private-file absence. Broader preservation and
adapter activation remain work.
Current private transactions collapse exact receipt-owned duplicate array
registrations at the first owned position; uninstall removes all exact owned
copies while preserving authored siblings and order. Unreceipted matches, edited
members and duplicate JSON keys still conflict. Historical v2/v3 recovery retains
its original strict single-member rules. Changed strict JSON may be reformatted;
JSONC comments are not silently discarded.
Explicit `--recover` (and `--recover --dry-run`) now recognizes exact same-harness
historical targets from compiled artifact history. Recovery finishes the original
install/uninstall target's immutable bytes and receipt, never substitutes the
latest adapter while interpreting an old journal. Unknown/altered targets, forged
artifact/receipt payloads, changed root or foreign preimages still reject. Normal
install/uninstall now previews the requested reconciliation against a read-only
virtual post-recovery tree. Check/dry-run reports `recovery_pending=true` and the
ordered deduplicated union of recovery and requested `changed_paths`, without
creating a lock or changing any journal/artifact. Apply retains one publication
lock across exact journal reauthorization, original-operation completion and
requested reconciliation. Changed/disappeared inspected intent is not adopted.
Successful automatic settlement reports `recovered=true` and
`recovery_pending=false`; a repeat reports neither pending recovery nor changes.
The two operations retain separate journals, not whole-workflow atomicity: an
interruption can leave original or requested intent pending for the next command.

Private bootstrap storage now has an internal native primitive for write-free
inspection, anchored private creation, cooperating locking, bounded exact-byte
reads and synced publication under `HOME/.config/mezzanine/bootstrap`. Its opaque
namespace follows the held vendor-root device/inode and effective UID, so aliases
share coordination; it does not supply artifact or recovery authority. Existing
HOME/config ancestor modes are not rewritten, and protected state remains
user-private under the Unix owner/mode contract. Captured intermediate directory
incarnations and the current lock-entry identity are revalidated before retained
writer ownership is reused; replacing an ancestor or lock fences the old holder.
Every compiled bootstrap intent now uses the private publisher: bare install,
check/dry-run, uninstall, and explicit recovery/preview all inspect both private
and recognized legacy intent. The standard process HOME is captured after adapter
and root admission, independently of vendor-root overrides; missing HOME is an
explicit private-state admission error, not a fallback location. Missing adapters
and retired Gemini do not perform HOME/root discovery. This owner captures existing or absent vendor
roots and protected HOME/base witnesses without writes. An absent root has no
speculative namespace or state access; install binds only after actual anchored
root creation and private HOME/base revalidation. Noop absent uninstall creates
nothing. Own shared-ancestor creation (such as fresh `.config/opencode`) advances
the private-base witness only with exact held-descriptor creation receipts, not
fresh path-based adoption. Replaced or externally appeared ancestors still reject.
Changed/appeared roots, HOME/config ancestors, and failed binding reject
without switching to legacy publication; created directories may remain after a
later binding failure. It retains both legacy and private cooperating lock domains;
new transactions journal privately, and compiled-authorized legacy recovery copies
exact original bytes durably before removing the old journal. Identical dual copies
after a copy-boundary interruption can resume; conflicting copies and changed
inspected source locations reject without choosing a winner. Final journal removal
checks exact accepted bytes/location. Ordinary private transactions use journal
version8; exact JSONC boolean-leaf ownership/removal uses version9. Both retain
explicit Private receipt placement and qualified prior receipt sources.
Recognized version-2 intent still finishes its original vendor destinations; it cannot
authorize private receipt targets. Unknown or altered version/placement/root/effects
remain non-destructive errors, not new artifact authority.
Explicit recovery freezes the accepted bytes/location before acquiring ownership
and revalidates that exact snapshot; absent roots or no pending journal are no-ops
without creating locks, directories or state. Both lock domains remain retained
for recognized legacy cooperation. New ownership receipts live privately as
`ownership-<harness>.json` under the held root namespace. The stable logical
`changed_paths` label `@mez-bootstrap-receipt/<harness>` identifies this internal
effect, not a vendor-relative path. Receipt payload schema remains 1; both copies
are inspected/compiled-qualified, disagreement refuses mutation, and private
publication follows artifacts before exact legacy marker removal. Interrupted
migration can resume with the same original intent. Generated-helper archives use
`archive-<harness>-<digest>` private files and logical changed-path labels
`@mez-bootstrap-archive/<harness>/<digest>`. Version 4 recomputes preservation authority
and rechecks dependencies before file effects and settlement; collision/failure
prevents overwrite, while confirmed effects are never implicitly undone. Historical
v2/v3 intent cannot invent archive effects. Broader shared-document preservation
remains unfinished. Vendor root and artifact-parent Unix
owner/mode metadata no longer add bootstrap eligibility rules: normal OS operations
decide access, and existing directory properties are not rewritten. Shared vendor
documents retain type/single-link/bounds/preimage/compiled-entry checks rather than
private-file Unix ownership policy. HOME and the managed Mez boundary stay protected;
shared `.config` is a pinned routing ancestor only. Unsafe legacy installer nodes or
private state still reject; actual filesystem denial is not bypassed or redirected.
No additional enable command, private-root override, or configuration option is
required for this private-journal routing.

Version-5 private journals added bounded multi-array ownership and
exact compiled Codex whole-file receipt projection while retaining v4 archive
dependencies, finite limits and preimage/root/source fences. Genuine v2-v4 recovery
keeps original target semantics, including old whole-file Codex deletion. Older
version labels cannot authorize new shared manifests or migration payloads; unknown
future versions reject. The legacy vendor-root publisher cannot emit new multi-array
intent: public bootstrap uses the private owner. Receipt payload schema remains 1;
there is no Mezzanine configuration schema change.

Version-8 private journals add exact compiled Claude mod ownership under only
`skills/mezzanine-telemetry/`, including receipt-owned generated-file archives and
repair. Versions 2–7 cannot acquire this ownership by relabelling a journal;
existing historical targets retain their original semantics. Receipt schema stays
1 and the Mezzanine configuration schema is unchanged.

`mez bootstrap claude` installs a ten-file observational mod at the selected
`CLAUDE_CONFIG_DIR` or `~/.claude` root's `skills/mezzanine-telemetry/`. The vendor's
ordinary skills-directory loader discovers it without `--plugin-dir`, a Mez
wrapper, fd3 or launcher helper environment. The literal SDK source pins the
installing Mezzanine executable for native helper observations and available
main-request counters. The documented [skills-directory loader](https://code.claude.com/docs/en/plugins/loading#name-conflicts)
loads the user copy ahead of a same-name project copy; managed locks, session-only
plugins and installed marketplace plugins can take precedence. Bootstrap leaves
those choices intact. A same-name higher-priority plugin can therefore leave
Mezzanine installed without its telemetry loaded. The [mod policy](https://code.claude.com/docs/en/plugins/mods/overview#turn-mods-on-or-off)
and vendor trust decisions still apply; installing this observational graph does
not change them. Revision2 adds bounded main-session classic lifecycle
observations and a static automatic title, without forwarding prompts, messages,
tool/error text or transcript paths. Presentation order is captured before
downstream completion; heartbeat and expense ownership remain separate. Exact
same-helper revision1/2 bundles upgrade through guarded historical source inputs.
Revision3 adds a narrow same-loaded-module clear handoff: a completed main
SessionEnd(clear) followed by the next main SessionStart(clear) for that same ID
uses the actually captured public predecessor and a fresh observer/lifetime.
The daemon still requires exact native creator/root and predecessor evidence;
old timers/senders cannot revive. Failed/lost replies or intervening starts do
not fall back to new admission. This does not qualify restart, reload, unpaired
resume/fork, inherited expense or exhaustive accounting after the transition.
Revision4 permanently closes the exact local observer owner on the bounded typed
authorization rejection (`{"observed":false,"terminal":true}`): it clears the receipt, cancels its timer and withholds
queued helpers or same-owner re-admission, while leaving vendor results neutral.
Only a strict matching-ID authenticated forbidden control error supplies this
outcome. Transport/malformed/deadline/other errors remain the neutral retryable
`{"observed":false}` and cannot close the owner. Exact revision3
bytes remain upgradeable through a frozen old client and guarded module recipe;
revisions1/2 retain their original source inputs. Local closure is not daemon
retirement, process death, renewal, executable attestation or billing evidence.
Revision5 retains failed final normalized request reports under the same live
observer. Active requests plus pending outcomes are bounded to sixteen; pressure
withholds new telemetry without evicting expense or altering the vendor request.
A later genuine main request retries at most one original pending report through
the existing finite typed-ACK client, never reruns the old request or borrows a
new observer. Stop/replacement/clear/proof closure fences retained work. Exact
revision1–4 source snapshots remain safely upgradeable. This is process-local
retry only: no timer, durable outbox, reload/restart or cross-process continuation
guarantee is added; absent later qualified callbacks, delivery remains unknown.
Revision7 also reports fixed missing-coverage reasons under the original native
observer: request readiness, finalized counters, durable acknowledgment, or finite
metadata capacity. `/status` shows `unavailable-claude-usage-*` even after earlier
requests reported expense; later successful reports retain that current-epoch
gap. Errors or cancellation without a final usage result disclose missing
counters without charging invented usage or changing the delegated result.
The diagnostic sender coalesces only queued/in-flight copies of each of four
fixed facts. A later genuine observation can redisclose after prior custody
settles. Its typed observation receipt is separate from durable expense ACKs.
It does not renew proof/leases, alter lifecycle/title, allocate source authority,
retry provider work or discard pending reports. Native generation/witness and
creator/root fences still apply, and a rotated observer starts its own diagnostic
state. Diagnostic delivery can itself be unavailable; it is not a durable spool
or an exact count of lost reports. Exact revision6 and earlier bundles remain
upgradeable; no new Mezzanine configuration or vendor enablement is added.
Revision8 accepts bounded UTF8 actual response-model labels, including the `@`
spelling used by some cloud model IDs. It preserves the label exactly through
native helper validation, durable replay and status. Labels stay limited to128
UTF8 bytes and exclude blank/control/bidi-control text; session, request and
source selectors retain their strict existing grammar. The original expense IDs
and all shared client bytes remain unchanged. Exact revision7 and earlier graphs
remain recognized for upgrade/repair, with no extra enablement or setting.
Revision9 correlates child steps only after a successful classic SubagentStart
under the exact current main observer. Child IDs retain their original session
in a bounded128-entry registry; failed starts cannot make an ID reusable by
another parent. A new genuine start is required after an observer epoch change.
Child request IDs use separate opaque domains; raw child IDs are not sent to
accounting. Main and qualified child requests share sixteen active/pending slots,
exact replays charge once, and child Stop leaves parent lifecycle and admitted
expense intact. Attribution uses the native creator's measured cwd, not a
virtual child cwd. Unknown, stale, cross-parent or uncorrelated resumed children
remain unavailable. No new enablement or configuration is added; revision8 and
earlier bundles remain recognized for upgrade and repair.
Focused Linux acceptance qualifies the unchanged bare-installed module's terminal
negative-proof cancellation/no-helper/no-re-admission behavior while its actual
Node creator remains alive; deterministic proof/lease aging is not wall-clock
expiry or actual vendor SDK cancellation. Separately, available Claude 2.1.289
retains seven real 10-second SDK proofs after SessionStart return during a full
60-second model-free offline command hold, with the real daemon timer worker
renewing the finite lease. Only test command/return instrumentation is added to
that isolated autoload graph. This is not quiescent interactive idle, reload,
cross-process source continuity, macOS or exhaustive vendor qualification.
An isolated production-helper fixture also qualifies exact already-committed
main-request replay and one distinct synthetic incurred request across this
same-creator clear: two observations retain one durable owner, without recharging
the replay or resetting prior expense. This is not live vendor billing or evidence
for unobserved historical/auxiliary requests or cross-process continuation.
Executable relocation remains fail-closed rather than guessed migration.
Installation does not override `disableAllHooks`, explicit
plugin disabling, managed policy, vendor trust review or authored statuslines and
settings. Support is best-effort; installed files are not proof of loaded runtime
telemetry, auxiliary/resume/fork coverage or exhaustive billing. Actual provider
and reasoning coverage remain unavailable when the documented SDK omits them.

Version-6 private journals added exact unreceipted private-bundle
adoption. Install can recognize a complete compiled Pi/OpenCode all-file bundle
at its enumerated private paths only when every file matches exact generated
bytes. It chooses the most-complete compiled match and checks all observed
preimages again; unknown/edited/partial unqualified slots and invalid receipts
still conflict. Surrounding authored files are not scanned or adopted. Shared
config/hooks, including unreceipted Codex hooks, are excluded. Unreceipted uninstall
does not adopt anything. Recovery keeps `adopted` distinct from previous receipt
provenance and re-proves the entire original byte bundle; versions 2–5 cannot
claim adoption. Existing receipt payload and Mezzanine configuration schemas do
not change. JSON `adoption_planned` includes qualified pending recovery/requested
intent; `legacy_adopted` stays false for read-only work and requires successful
actual settlement. Matching-current adoption reports `repaired`; historical
refresh reports `upgraded`. No adoption report implies vendor loading, enrollment
or accounting support. Failed publication may create coordination lock/private
directories, but cannot overwrite changed artifact preimages or publish success.

Version-7 private journals introduced OpenCode revision-7 JSONC
registration. The selected root uses existing `tui.jsonc` before `tui.json`; both
file preimages/absence are captured and revalidated. Ownership is one exact
`/plugin` member, or `/tui/plugin` when loader normalization makes an existing
nested list effective and no root plugin field overrides it. Four fixed compiled
variants do not authorize arbitrary custom paths. Receipt-qualified moves retain
both documents and every authored setting; repeat is byte-exact. Upgrades
recognize all four shipped revision-7/8 layouts independently of the newly
effective filename/pointer, so an upgrade can move the owned registration without
rejecting a known predecessor. Helper references still require exact compiled
identity; this does not authenticate arbitrary relocated executables. Token edits
retain comments, BOM, CRLF, Unicode and unrelated whitespace; JSONC allows trailing
commas, not JSON5 expressions or duplicate keys. Finite parsing and final semantic
revalidation precede publication. Genuine older strict operations stay frozen;
older labels cannot acquire JSONC authority. Version-6 unchanged TUI/client payload
fingerprints fail closed on source drift until reviewed snapshots exist.

Loader evidence is the maintained `anomalyco/opencode` `v1.17.13` source
`packages/opencode/src/config/{tui,paths,parse}.ts`, plus official TUI documentation.
These are adaptation evidence, not release pins or live-loader certification.
The vendor merges root JSON before JSONC, normalizes nested TUI fields and deduplicates
plugin origins. Bootstrap edits only the selected explicit/user root; supplemental
`OPENCODE_TUI_CONFIG`/project sources are not scanned or rewritten, and installation
does not attest that vendor policy permits loading or telemetry/accounting works.

### Normalized observational hook helper

`mez -S /absolute/control.sock harness-event` reads one JSON envelope from stdin:

```json
{"operation":"presentation","launch_token":"<private capability>","generation":1,"external_session_id":"bound-run","data":{"sequence":1,"state":"running","title":"Task"}}
```

The token is stdin-only, never an argument or persisted configuration value.
Existing lifecycle operations use a primary-authorized launch or an independently
qualified ordinary producer, according to their control contract. The helper
maps only `register`, `renew`, `end`, `presentation`, `helper-presentation`,
`helper-observe`, and `usage` to their restricted
external-agent operations; `data` uses the documented RPC fields without identity
overrides, arbitrary methods, paths, prompts, transcripts or nested vendor payloads.
It authenticates the Unix daemon's same-user peer and does not initialize a client.
Input is capped at 64 KiB with a 250 ms EOF deadline; the single exchange is bounded
to 500 ms. No retries or subprocesses are issued. Telemetry errors produce neutral
`{}` output and success exit, without echoing credentials or daemon errors.
The envelope must be a JSON object with recursively unique decoded fields,
including `data` and nested counters. Duplicate or escaped-alias fields are
rejected before normalization; positional arrays, malformed/trailing JSON and
excessive depth are not alternate envelope formats. Rejected input is not sent
to the daemon. Valid Unicode and null/zero/absent counter semantics are preserved.
Internal replies must be duplicate-safe JSON-RPC 2.0 with the matching ID, an
object result and no error member or buffered trailing frame/data. A rejected
reply does not cause a retry or appear on vendor stdout. Neutral `{}`/success
exit and a generic JSON-RPC acknowledgment are not durable usage ledger receipts.

`helper-presentation` is narrower: the private handle must already belong to an
independently enrolled ordinary producer, and the caller must be its kernel-qualified
direct child. Only generic sequence/state/title metadata is accepted; no PID/pane
claim, initial parent enrollment, accounting, observer, renewal or retirement is
granted. Pi lifecycle-owned sequences reject generic helper updates. It shares
the server's bounded asynchronous native-admission path; helper exit does not
retire the producer. This does not yet install or activate ordinary hook adapters.

`helper-observe` avoids private credential handoff for the same existing-producer
case. Its separate strict envelope is:

```json
{"operation":"helper-observe","harness":"pi","generation":1,"observer_witness":"<64 lowercase hex characters from enrollment>","external_session_id":"bound-run","data":{"sequence":1,"state":"running"}}
```

The original public generation and observer-instance witness come from the
producer's enrollment response; the adapter must bind them to the callback,
never fetch the newest generation or witness for
delayed work. No token is accepted or returned. Missing/ambiguous daemon-owned
targets fail closed, and native direct-parent, writer and actor fences still run.
This does not enroll a parent or grant observer, renewal, retirement or usage
rights. Callback sequence serialization remains the producer adapter's job.
An already-enrolled shared client can capture the frozen envelope with
`captureHelperObservation(state)`, sharing its generic sequence owner. Capture
does no implicit enrollment, delivery or helper spawn and is not a receipt. The
adapter must keep capture and child dispatch ordered; it cannot refresh the
generation/witness after loss or replacement. The exact `mez harness-event` mode
uses MEZ discovery before ordinary HOME/config/runtime CPU initialization, so it
works with a cleared environment apart from those routing hints. Missing/invalid
routing remains neutral without guessing a default socket. Explicit `-S`/other
CLI forms retain normal configuration startup.

Curated one-shot APIs whose stdin is closed can invoke the separate fixed hidden
`mez harness-source <capsule-json>` mode. Its only public capsule fields are
`external_session_id`, `observer_instance` (opaque ASCII IDs, max128) and
`session_boundary` (`startup`, `resume`, `clear`, `fork`), plus optional positive
JS-safe `predecessor_generation` for explicit same-run observer handoff. Missing
predecessor means initial admission; explicit null/zero/unsafe/ambiguous selectors
reject. The capsule is bounded
at4096 bytes, rejects duplicate/extra fields and contains no credential. Source
profile/harness/method/labels are fixed code; pane/socket come only from standard
MEZ/MEZ_PANE routing hints and the daemon still requires real native creator proof.
Exact argv bypasses HOME/config/CPU startup, never reads stdin or starts/retries
vendor work, and makes one 500ms same-user peer-verified Unix exchange. It returns
only a strictly projected matching public admission receipt (no `launch_token`)
or `{"registered":false}` on unavailable/rejected telemetry, with no stderr/error
content. The curated API must capture that output and preserve its original
middleware result; it is not a vendor hook response, live observer proof or a
durable usage acknowledgment. No arbitrary RPC/endpoint/pane/PID/role fields are
accepted. This supplies transport, not automatic adapter/mod installation,
idle/reload/session continuity, permission-policy bypass or accounting support.
The fixed argv capsule projects only that explicit predecessor into the existing
curated-enroll method; it cannot infer a current observer or revive a retired
namespace. Handoff receipts must carry a generation greater than the predecessor
and observer epoch above1. A transport primitive is not installed reload policy.
The same fixed helper has a distinct public proof capsule
`operation:"curated-heartbeat"` plus original `external_session_id`, positive
JS-safe `generation`, 64-lowercase-hex `observer_witness`, and positive JS-safe
`sequence`. No TTL/state/role/credential/endpoint fields are accepted. It returns
only matching observed/sequence/changed data or `{"observed":false}`. This is
native-qualified original-epoch observer freshness, not direct lease renewal or
billing; equal sequence replay cannot refresh freshness. A source adapter must pin
its captured scope, serialize/bound timer work and keep SDK timer output private.
The reusable literal curated source does this with one in-flight fixed helper,
no queued timer work, finite helper timeouts and recurring original-epoch proofs.
It preserves the middleware result/error and never prints raw receipts or
re-enrolls on failure. Its real init-only SDK fixture qualifies three recurring
callbacks while SessionStart is held open, not a deployed adapter, post-start
idle lifetime, reload/session replacement or accounting. Bootstrap does not yet
install or enable this source.
Its separate successor body requires a captured prior public receipt in the
fixed `previousObserver` caller scope and a fixed new opaque instance. It freezes
prior session/run/agent/epoch/witness, sends only its explicit predecessor, and
accepts only a matching same-run reply with fresh generation/witness and epoch+1.
Missing/incompatible prior evidence never falls back to initial admission. New
timer closures pin only the successor selectors and start their own sequence;
old callbacks cannot adopt them. The caller still owns module/timer teardown and
handoff persistence; this does not install an automatic reload mechanism.
Explicit owned variants also require a retained `observerLifetime` from the pure
one-slot owner. It holds one pending exact ticket and one active immutable public
receipt/timer, with no queue. Duplicate calls cannot admit or schedule twice;
stop is permanent, clears public state before cancelling the exact timer, and
queued callbacks check their original ticket before launching a helper. Late
admission/attachment, reentrant teardown and cancellation errors remain neutral.
Already-started helpers may still settle under native daemon fences; local stop
does not retire a namespace or declare the vendor dead. The owner does not persist
state or install an automatic module/session teardown policy.
The separately rendered literal module retains that owner across main classic
SessionStart/SessionEnd callbacks, projects only known inert metadata, filters
child/foreign events and preserves exact middleware results/errors. Start order
is captured before downstream waits; matching end captures its exact active or
provisional owner rather than finding a newest observer afterward. Different
vendor session IDs stop old local work and use a fresh owner after successful
downstream start. Same-ID duplicates coalesce, and same-ID end/restart stays
unavailable rather than reviving an owner. This is source/SDK qualification, not
bootstrap installation, reload persistence or durable usage identity.
Ended main IDs remain fenced in a runtime-only set capped at128; exhaustion
withholds new admission without eviction. Pending same-ID callbacks share the
exact owner, including after end-before-bind, and stale/failing cleanup cannot
stop that owner while a newer pending callback still retains it.
Separate ordinary Node SDK-shim/native-actor coverage does test proofs after the
callback returns, bounded simulated freshness loss with a live producer, and
native producer death despite a surviving pane shell. It does not promote that
fixture to actual Claude idle/deployment or a wall-clock longevity guarantee.

This is not vendor hook installation or certification. Adapters must privately
deliver existing run credentials, normalize content-free released payloads, serialize
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

Help/version, completion, bare `mez config`, and `mez config default` are
synchronous displays available without HOME or usable primary configuration.
`mez config path` and `mez config validate [PATH]` also run before configured
runtime initialization and never migrate, publish config, or clean up sockets.
Implicit selection requires HOME and rejects multiple primary files; explicit
validation consults only the supplied file. Validation reports the original
document against the current schema with exit zero even for `valid: false`;
selection and file I/O errors exit one. All other management and runtime
commands retain strict primary security/CPU validation. See
[configuration recovery](../configuration/overview.md#recover-from-startup-configuration-errors).

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
The next internal transition uses the retained endpoint's configured transport. It retains
the exact frontend and independent connection lease, verifies the server ID,
and negotiates a bounded codec without sending application initialization.
Configured port mapping stays with the retained endpoint and no longer blocks
an otherwise qualified protected direct address. Loopback regressions verify
shared identity and sibling byte transfer with this policy enabled.
Startup and connection preflight share possible routing evidence from direct
addresses, configured relay routes or configured network lookup. The protected
endpoint identity never changes. Loopback tests cover known direct peers with
custom relay/lookup policy enabled, not external relay or DNS reachability;
connection failure never switches to another endpoint owner or replays setup.
An internal host-only initialization transition sends one owner-authenticated
request and validates correlated observer/host-only settlement. Private proof
and raw peer responses stay inside the owner; only allowlisted facts are retained.
An internal host-list exchange now returns bounded validated lease summaries
through exact-handle IPC using the fixed read-only host method. It exposes no
device proof or private principal/checkpoint metadata and retires only the
management connection. Real-host fixtures qualify listing beside a live sibling
without session allocation.
Ordinary `list --iroh-profile NAME` now reuses an authenticated active broker,
after checking the current outbound veto. Only missing or refused discovery
retains direct fallback; unsafe discovery, protocol errors and failed broker
operations do not create a competing endpoint. Host-invitation list and force-kill
now pair through the active owner, then use fresh readiness for their fixed
management request. Scope, expiry, profile pinning and destructive role checks
precede redemption. Post-pair failure never restores direct fallback or replays
the invitation; the original kill target/key remain retained. Fresh re-pairing
still supersedes prior endpoint trust; it does not preserve revoked authority.
Automatic startup for management remains separate.
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
fallback or replay. Initial absent/refused discovery can elect a first owner for
host invitations with configured routing, retaining the exact child before any
redemption. Failed readiness preserves proof and cannot restore direct eligibility.
Ineligible routes retain their existing path before startup.
Explicit invitation X11 now validates its final offer and prepares client-local
credentials before redemption. Pairing/reconnect loss cleans those credentials
without replay; successful attachment preserves trust/takeover and the original key.
Legacy pairing remains separate.
Paired-profile `kill --force TARGET` reuses an authenticated active broker too,
preserving the exact target and invocation key through a fixed host kill request.
The host still enforces destructive authority and visibility. Only correlated
revoked-lease evidence reports success; uncertain exchanges never replay or fall
back to a competing endpoint. Invitation administration uses the same management
settlement after protected pairing, not a new destructive authority channel.
A paired-profile attachment setup adapter now preserves prepared routing
and invocation keys through an existing authenticated broker, checking current
outbound policy and protected profile role. Missing/refused discovery leaves the
direct path eligible only before connection; discovery loss during connected
readiness is terminal. Readiness/setup failures never acquire a competing identity
or replay creation. Ordinary `attach` and `new` now reuse an active broker for
eligible paired host profiles, with guarded terminal output and restoration.
OS signals retire only the local frontend; terminal Ctrl-C remains forwarded input.
Initial absent/refused discovery now permits elected first-owner startup for
profiles with configured route availability, using the running binary and retaining the
exact child through setup and foreground exit. Startup failure does not switch to
direct setup or replay creation. Exited children are observed/reaped; live children
are not killed on frontend exit, and reaping remains best-effort after disposal.
Unqualified routes retain direct eligibility before startup. Qualified paired-host
X11 first attachments also elect the shared owner rather than taking its identity
lock directly; failed readiness retains exact child evidence without direct fallback.
Paired-host `--x11`/`--x11-trusted` attachments can now reuse an active broker,
preserving takeover intent and the original session operation key. Final offer
preflight precedes local credential generation; only the fake offer cookie crosses
IPC. One matching-capacity opener and prepared credential owner survive through
foreground retirement, with channels disposed before credential cleanup. Packet
setup and foreground retirement use independent finite budgets. Setup uncertainty
never authorizes endpoint fallback or replay. Invitation X11 shares the same local
credential lifetime after pre-redemption preparation; physical X and macOS
acceptance remain unqualified.
Errors after writing never automatically replay initialization. Session
creation/attachment uses a separate internal transition retaining the original
intent/key and validating exact client/session/active-lease evidence. A loopback
host fixture qualifies two sessions while the first remains attached and sibling
control survives retirement. Separate explicit process fixtures qualify ordinary
two-terminal concurrency and invitation-first startup. Real-host X11 byte forwarding
and clipboard-v2 coexistence pass across all codecs with a synthetic local X peer;
another explicit process fixture exercises overlapping X11 applications through two
ordinary CLI frontends with generated fixture credentials. Retiring one frontend
closes its active local relay and removes private artifacts while its sibling
continues bidirectional X traffic and terminal input. Physical X SECURITY issuance,
server-side revocation and macOS acceptance remain unqualified.
Shared-owner fault fixtures cover real committed creation with reply loss, a
normal-correlation negative control, deliberate same-key recovery and changed-input
rejection with spare capacity. An actual broker-crash fixture requires frontends
to retire while committed host sessions survive; replacement preserves the paired
identity and explicitly attaches without replaying Create. These checks are
automated loopback/process acceptance, not exhaustive network or platform testing.
An internal self-detach API now consumes the exact initialized primary frontend,
preserves its mutation key and validates exact-client settlement. It exposes no
sibling target and ends the settled broker pipeline after reply delivery. Errors
are not replayed. Ordinary host-profile administrative detach uses its separately
validated explicit session/client target and temporary primary authority; it is not
the target-free self-detach API.
Optional version-one events now remain owned by the initialized session and its
exact connection lease. Setup validates the preface within a deadline; later
versions remain rejected by this v1 transition. Explicit X11 uses separately
validated route admission. Real-host fixtures qualify event availability after
sibling retirement; frontend event polling and ordinary attachment have their
own ownership and cancellation coverage.
A separate internal clipboard-session admission now requires explicit version-two
primary intent and true capability in the validated initialize reply before stream
acceptance. Internal supervision selects this path for explicit version-two intent
and retains typed effect ownership on the exact connection. Redraw-only polling
and foreground entry reject these sessions rather than discard effects. Real-host
fixtures qualify bounded Unicode item delivery across all codecs and sibling
survival after self-detach, not host clipboard writes or ordinary CLI attachment.
A separate internal X11 admission now retains exact correlated route authority
for an explicitly requested primary offer. Missing capability, changed trust mode
or invalid route proof rejects; proof stays outside local summaries. The internal
listener now selects dedicated supervision for authored offers after host-only
handling. Ordinary CLI X11 preparation remains gated pending attachment-lifetime
qualification.
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
cancels handoff/relay. Local handshake keeps its reservation deadline. Afterwards,
idle demand retains the same stream, permit and occurrence without reopening it;
local EOF retires only that channel and premature frontend bytes reject. Remote
stream arrival starts one finite preface/setup-delivery deadline, never extended
by partial packets. An integrated synthetic fixture qualifies
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
connect and initial delivery share a deadline starting at the first setup byte.
Idle demand retains the same owned stream until EOF or cancellation without local
dialing or automatic retry; incomplete setup remains deadline-bound.
Subsequent bytes and half-close
remain caller-owned. Synthetic local TCP fixtures qualify both byte orders,
rejection and cancellation, not a physical X server or ordinary CLI activation.
The prepared credential lease remains separately owned by the attachment caller.
A staged client supervisor now shares one opener across a finite collection of
directly owned channel futures. Cancellation and abandonment dispose those
streams; errors stop supervision without retrying the channel operation.
EOF before setup retires supervision without reopening. Only completed setup and
bidirectional application relay can admit fresh demand without replaying prior bytes.
Deterministic ownership tests qualify this boundary, not attachment-lifetime
credential cleanup, terminal restoration or ordinary CLI activation.
A staged lifetime coordinator now disposes channel futures before credential
cleanup and gives the foreground a persistent stop request plus a finite restoration
budget. Causal failures retain precedence. Deterministic tests qualify ordering,
timeout and abandonment, not physical terminal restoration or generated credential
revocation; ordinary routing remains unchanged.
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
Cancellation or control failure disposes channel work and listener publication.
A combined synthetic fixture now exercises internal listener dispatch, initial
snapshot, discovery, production channel opening and client-local substitution,
then checks continued control and dedicated-publication cleanup. This is not
real-host X route provisioning, physical X behavior or ordinary CLI X11 activation.
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
The acquiring process explicitly releases its election on guard disposal so
fork-before-exec duplicate descriptors cannot prolong normal release. A failed
contender or an inherited guard cannot unlock a live owner's election; inherited
guards also cannot authorize startup. Release targets the held object even after
path replacement, never a reopened path or unlink. Unexpected unlock errors use
ordinary descriptor-close fallback; arbitrary host-I/O liveness is not guaranteed.
The internal readiness connector authenticates the Unix peer and negotiates a
bounded hello while retaining the exact local stream. Discovery checks private
root/socket identity without creating or replacing state; socket existence alone
does not establish readiness or remote session authority.
The hello token `mez-outbound/2` is the exact whole local-wire contract epoch,
not the product release or remote transport version. It requires the current
conditional-view schema; old `/1`, future epochs and missing evidence are rejected
before broker-side profile setup, pairing, session creation or terminal effects.
There is no old-protocol or feature fallback. Connected readiness failure never
authorizes direct endpoint use, replacement startup, or operation replay.
An EOF can mean unavailable **or** incompatible readiness; it does not establish
the cause of an earlier conditional-snapshot error. Replacing the installed
executable does not replace a detached broker. See [outbound broker upgrade
guidance](../operations/iroh-production-operations-and-rollout.md#outbound-broker-wire-upgrades)
before coordinating explicit retirement with its owner and sibling attachments.
Election, discovery and listener cleanup compare both device and inode through
no-follow observations and retained directory descriptors. Native stat device
numbers use std's u64 identity representation: full-width unsigned on Linux,
sign-extended from signed 32-bit on macOS, including high-bit device values.
Cleanup preserves observed replacement sockets; its final check/unlink remains
a cooperative same-user boundary, not atomic compare-and-delete.
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
