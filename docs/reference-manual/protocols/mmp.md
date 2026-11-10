# `mmp/1` local message protocol reference

## Purpose

Specify Mezzanine Message Protocol version 1 (`mmp/1`), the local service for
agent discovery, direct/group messaging, presence, correlation, and task
status. It is separate from the `mezctl/2` direct-session and `mezctl/3`
persistent-host control endpoints: MMP never creates panes, changes layout, or
performs multiplexer control. The [MMP section of `SPEC.md`](../../../SPEC.md#12-local-message-passing-protocol)
is normative.

## Transport and framing

MMP is local to a Mezzanine session by default. Implementations provide a
reliable, ordered local transport—normally a Unix-domain socket in a
user-private runtime directory. Loopback TCP is optional, must be protected by
an unguessable session capability, and is disabled for remote access by
default.

Frames are UTF-8 JSON values preceded by an ASCII header block:

```text
Content-Length: <decimal-octet-length>\r\n
Content-Type: application/vnd.mezzanine.mmp+json; version=1\r\n
\r\n
```

`Content-Length` is the JSON body's octet length. Receivers reject missing,
invalid, negative, or oversized values and ignore unknown headers.
The physical header has a separate 8192-byte maximum including its final
`\r\n\r\n`; unterminated headers fail when a terminator can no longer fit.
Body bytes and subsequent buffered frames are not part of this header budget.

## Envelope and identity

The normative versioned message envelope is an object with these fields:

| Field | Meaning |
| --- | --- |
| `protocol` | Exactly `mmp/1`. |
| `id` | Globally unique message ID in the session. Recipients treat it as an idempotency key. |
| `type` | One of the message types accepted by the endpoint below. |
| `time` | RFC 3339 or documented monotonic time. |
| `sender` | Authenticated sender identity. Registered agents include `agent_id`; pane, window, role, capabilities, and the bounded generated `objective` may be present. |
| `recipient` | Agent, pane, window, session, role, capability query, or group target. |
| `scope` | Optional top-level audience: `project`, `session`, or `null`; omitted or null means `project`. This is separate from the recipient selector. |
| `correlation_id` | Related request ID, or `null`. |
| `ttl_ms` | Time-to-live in milliseconds, or `null`. |
| `content_type` | Payload media type. |
| `payload` | The application payload. |

The current endpoint enforces this full shape for `send`, `task_status`, and
`task_result`. Its service operations use reduced, operation-specific request
objects instead. Registration `hello` contains `protocol`, `type`, an optional
non-empty `role`, optional `capabilities`, and optional top-level `objective`;
an omitted role defaults to `default`. The service assigns the effective
identity in `welcome`. Raw `hello` assigns neither pane/window identity nor
trusted project membership. Client-supplied project fields cannot establish
that membership; the trusted runtime registration/rebinding path owns it.

Runtime-authored subagent lifecycle envelopes use IDs shaped as
`<turn-id>:task_status:<state>:<sequence>` and
`<turn-id>:task_result:final:<sequence>`. The final component is the durable
MMP acceptance sequence. It keeps lifecycle IDs unique across snapshot restore
and distinguishes repeated occurrences of the same task state.

These reduced service-operation shapes are an implementation conformance gap.
`SPEC.md` requires every message envelope to carry the full envelope and only
allows a pre-registration `hello` sender to omit `agent_id` or use a
provisional ID. The current full-envelope parser also requires `time` to be a
non-empty string but does not yet validate the normative RFC 3339 or documented
monotonic-time grammar.

After registration, the service—not the sender—validates identity against the
authenticated connection. Mismatched sender claims are rejected unless a
documented trusted bridge rewrites them. MMP currently preserves accepted
non-reserved extension fields at the top level, as required for forwarding by
the MMP contract; this is an explicit exception to the shared
`extensions`-object convention.

Registered sender identities may project an optional `objective`: the agent's
bounded, model-generated statement of current work for peer discovery. Raw
`hello` and `presence` requests publish it at the **top level**, not inside a
`sender` object. The field is additive to `mmp/1`; it does not change the
protocol version or non-reserved extension forwarding. A supplied string must
normalize to non-empty text of at most 2097152 bytes (the agent-shell prompt
ingestion bound), whitespace-collapsed to one line and free of control
characters. Multi-line text is collapsed rather than rejected solely for
spanning lines. Missing or null objective does not clear a published value.
An objective-only refresh with no value is a no-op, but a raw `presence`
request still updates status and its timestamp even when objective is absent.
`welcome.identity` and `discover_result.agents` project the registered
objective; raw `presence` returns an `ack`, not an identity projection.

### Registration and presence request shapes

```json
{"protocol":"mmp/1","type":"hello","role":"worker","capabilities":["docs"],"objective":"Checking protocol documentation."}
```

The response is `{"protocol":"mmp/1","type":"welcome","identity":{...}}`.
Use the assigned `identity.agent_id` in later full envelopes. On that same
registered connection, a presence update can be:

```json
{"protocol":"mmp/1","type":"presence","id":"presence-1","status":"busy","objective":"Validating protocol examples."}
```

Accepted statuses are `available`, `busy`, `blocked`, and `offline`; omitted
status defaults to `available`. Dispatch updates status and `updated_at_ms`
using service time, then applies any supplied objective. It does **not** update
capabilities; those are registration metadata. The response is:

```json
{"protocol":"mmp/1","type":"ack","message_id":"presence-1","queued_recipients":0}
```

The request ID is optional; without it `message_id` is null. A heartbeat also
returns this ack shape and updates liveness time without changing status.

For native pane agents, registered role and populated capabilities describe the
stable native registration, not the currently resumed conversation kind. An
independently resumed root conversation can therefore have discovery kind
`primary` while its original role remains `worker` or `explorer`. Trusted generic
runtime refresh preserves that exact routing metadata only for the matching
actor-owned pane/session; explicit creation conflicts and project/pane/window
checks remain strict. These labels grant no parent, cleanup, scope or approval
authority. Resume neither clears the directory/cursors nor creates another ID.

## Message types

| Type | Direction and meaning |
| --- | --- |
| `hello` | Client registers with the service. |
| `welcome` | Service confirms registration and assigned identity, including the registered `objective` when published. |
| `discover` | Query agents by identity, pane, window, role, status, or capabilities. |
| `discover_result` | Discovery response carrying matched sender identities with their published `objective` values. |
| `send` | Submit application payload for a recipient or scope. |
| `mmp.receive` | Poll a subscribed recipient for a delivery batch; optional `limit` defaults to 100. |
| `transport/receive` | Compatibility alias for `mmp.receive`. |
| `deliver` | Service delivers a batch containing `cursor` and sequenced `messages`. |
| `ack` | Service response acknowledging sender-side acceptance, or recipient request advancing a subscription through `sequence` (or compatibility field `last_sequence`). |
| `error` | Structured protocol or delivery failure. |
| `presence` | Update status and presence time, optionally publishing a top-level `objective`; returns an ack. It does not update capabilities. |
| `heartbeat` | Prove connection liveness. |
| `task_status` | Report task state. |
| `task_result` | Report task completion. |

Types outside the baseline list must use a reverse-DNS or URI-like namespace.
The current endpoint recognizes that namespace grammar during validation but
does not dispatch extension types, so it rejects them as unsupported endpoint
operations. Namespace syntax alone does not advertise extension support.

### Task payloads

`task_status` and `task_result` use the full envelope described above, with
`content_type: "application/json"` and an object payload. Their required fields
are validated before acceptance:

| Type | Payload fields |
| --- | --- |
| `task_status` | Non-blank string `task_id`, string `state`, and string `summary`. States are `queued`, `running`, `blocked`, `succeeded`, `failed`, or `cancelled`. Optional `progress_percent` is null or an integer from 0 through 100. |
| `task_result` | Non-blank string `task_id`, boolean `success`, string `summary`, and string `output`. Both strings are required even when empty. |

For example, these are payload objects, not complete requests:

```json
{"task_id":"task-1","state":"running","progress_percent":25,"summary":"Checking protocol examples."}
```

```json
{"task_id":"task-1","success":true,"summary":"Checks passed.","output":"Two focused tests passed."}
```

Malformed payloads produce `invalid_envelope` without queueing the message.
Acceptance acknowledges a report, not independent verification of the reported
task outcome and not authority to execute work.

## Delivery audience and recipient selection

Raw `send`, `task_status`, and `task_result` envelopes accept optional top-level
`scope: "project" | "session" | null`. Omission or null resolves to `project`;
other values are invalid. Project delivery requires the authenticated sender's
trusted project membership and reaches only matching identities. A sender
without membership fails closed, not by widening to the session. A
cross-project direct recipient is indistinguishable from an absent or
unavailable target. `session` is an explicit visibility widening request, not
authorization to execute work. The service records the resolved audience at
acceptance; later runtime membership changes do not rewrite queued audiences.

Acceptance also freezes the exact eligible recipient registrations. A late
registration, previously offline peer, or newly matching sparse metadata cannot
acquire old fanout. Reusing an opaque ID after retirement starts a new interval;
neither live nor historical reads inherit the old owner's mail. Current
availability, selector and audience checks still apply, so acceptance does not
grant permission or bypass project isolation.

Retiring a reusable/native identity removes its discovery, presence and
subscription, but preserves shared canonical envelopes, acceptance receipts and
other recipients' offer/cursor evidence. In particular, retirement of a fast
fanout recipient cannot delete a slow recipient's unread copy. The existing
delivery floor and frozen registration bindings prevent a replacement owner or
late matching registration from acquiring old mail. Direct-only retained mail
may remain as bounded evidence even after its recipient retires; it is not
evidence of delivery or a terminal disposition. Unsatisfied expired/retired
obligations remain charged until explicit terminal maintenance settles them,
never silently treated as a successful ACK.

The lower `MessageService::settle_delivery_outcomes_at` API records one typed
`expired` or `recipient_retired` disposition in each pre-reserved recipient slot.
Expiry uses the original strict TTL boundary; retirement means the exact
registration interval ended, not merely offline presence. First reason/time
wins, and clock rollback cannot revive terminal mail. Terminal slots never
become recipient ACKs, even through a later cumulative ACK. A genuine later
offered endpoint can advance across a terminal prefix without consuming its
outcome.

`delivery_outcomes_for` exposes at most 100 payload-free receipts per call to
the authenticated original sender interval. `acknowledge_delivery_outcome`
releases only an exact receipt after caller-owned handoff commits. Reused IDs
cannot consume old receipts; unknown legacy sender intervals remain unclaimable.
Outcomes remain reserved for ten minutes from first service observation, then
explicit maintenance releases them. Retries/rollback do not renew the horizon.
Shared custody releases only after every recipient ACKs or its terminal receipt
is consumed/ages out; optional replay may retain terminal tombstones afterward.
This is wall-clock-based, not monotonic elapsed-time or crash-durability proof.

**Integration boundary:** the product timer invokes this maintenance after
native commit reconciliation. Only an exact original live accepted action can
consume its handoff, as described below; other owners retain the bounded
receipt until expiry. No raw wire action, ordinary mail, model wake, task retry
or fabricated successful delivery is introduced by this contract.

Accepted same-runtime custody is independent of optional replay history. The
pending and replay references share the original immutable occurrence; replay
FIFO eviction cannot discard unread direct mail or a slow fanout recipient's
obligation, acceptance receipt, offer evidence or required sender provenance.
Sequenced receive/fanout returns custody-only mail in original acceptance order.
Unsequenced replay/history views remain optional bounded history. Subscribing
late cannot skip accepted unread mail; repeated subscriptions preserve their
cursor. Explicit reconstruction can inspect committed retained evidence without
redelivering it as new work.

Internal pending ceilings are 1000 occurrences, 1 MiB unique payload, 4096 frozen
recipient slots and 256 slots per exact recipient interval, independently of
the replay window. Fanout charges the shared payload once; committed slots are
conservatively charged until the last recipient commits or its terminal receipt
is released. The last exact offered ACK releases pending custody, not a remaining replay
reference. Capacity is reserved before assigning sequence/success. These limits
do not add a config knob, disk spool, fair deferred-send scheduler, or durable
acceptance. The existing one-second product delivery timer also maintains
future TTL expiry, exact recipient retirement and unconsumed terminal horizons
when live fanout is empty. TTL-free merely offline mail and already released
slots do not independently keep terminal maintenance ticking.

For an exact original live `send_message` action, the timer commits one
payload-free, summarizable runtime reference before consuming its receipt.
Compaction can retire settled facts without recreating their consumed receipts.
Original sender registration, generated turn/action ID, acceptance sequence,
task context and current conversation must agree. Ownership is captured at
actual native acceptance, survives provider batch replacement and is bounded
by at most 1000 canonical pending occurrences. It stores original identities
and occurrence references, not payloads, and does not restore task authority
after restart. The original action remains accepted. This fact does not act
as peer mail, resume a wait, wake a provider or retry effects.
Ended/replaced/unprovable owners retain the bounded receipt until ten minutes
from first observation, then maintenance releases it without handing it to
another conversation. Exact eligible occurrence queries inspect at most 100
outcomes per sender/sweep; earlier unowned receipts cannot hide a later live
owner's handoff. Request-aware runnable recovery, fairness and crash-durable
acceptance remain separate work.

Native receive receipts, including invisible media commit proofs, share the
1,024-entry and 16 MiB logical source-byte live bounds. Source charging includes
512 bytes per record plus identity, recipient, pane, conversation, turn, label,
content-type and retained-payload UTF-8 lengths. Exact planned-turn batch
admission precedes canonical context mutation; overflow leaves mail unacknowledged.
Snapshots, rollback adoption and legacy reconstruction enforce the same bounds.

Native receive admission seals same-runtime proof only after canonical context
storage. That proof survives Stop, context cleanup and failed-backing moves;
restored display projections and historical labels cannot claim it. Retiring a
pane/conversation suppresses its old display while retaining unacknowledged
proof until transport settlement. It cannot repaint a reopened conversation.

A later native input cannot cumulatively ACK an earlier offered input that
never committed. Proven input waiting on a prefix is protected from false TTL
failure and excluded from native fanout/prompt construction, so it neither
replays as new work nor hides genuinely unread mail behind a fanout limit.
Only a reconcilable prefix or finite terminal work keeps the maintenance timer
active. At exact native registration retirement, only proven own offers receive
positive ACKs; genuinely unsatisfied recipients keep their terminal obligations.
This adds no wire ACK action, task restart authority or acceptance-atomic crash
persistence. Unacknowledged same-runtime proof is not restored as task authority
from a snapshot's presentation outbox.

Trusted in-process callers can narrow each custody budget using
`PendingDeliveryLimits` with `MessageService::with_pending_limits`. Values must
be positive and cannot exceed the ceilings above. No wire/config control is
introduced. Policy is destination-owned, not saved in schema9 message state:
`from_snapshot_state` explicitly uses production defaults, while a narrowed
destination must use `from_snapshot_state_with_pending_limits` with its policy.
Over-cap current or reconstructed legacy custody is refused, never truncated.
Complete snapshot capture also fails if retained source content is unavailable;
it never omits accepted pending mail or substitutes empty payloads. This error
propagates through session snapshots and host checkpoints. Current production
custody remains resident; this failure boundary does not enable disk spill or
add a restart-delivery guarantee.
Trusted internal cache release and reload additionally require the exact live
root's original positive storage adoption. Reload verifies the complete source
fingerprint and frozen occurrence fields, moves decoded bytes without another
payload copy, and publishes into that same canonical occurrence. It changes no
TTL basis, recipient rights, delivery offer, ACK or acceptance receipt. A saved
snapshot cannot recreate this qualification.

An internal fresh-store opt-in now connects these APIs to bounded actor paging.
It separates logical mail (10,000 occurrences, 64 MiB payload, 40,000 obligations)
from the unchanged 1,000-allocation/1 MiB canonical cache. Per recipient interval,
1,024 obligations and 8 MiB payload apply; combined logical metadata has an 8 MiB
ceiling including worker, cache-index and reader headroom. Every limit can be
narrowed by trusted construction, independently of physical store capacity.
Live delivery references, unpublished worker allocations and decoder reservations
remain charged after cache release. At most 64 deferred readers and 64 KiB of
reader descriptors wait on the existing root lane; cancellation before issuance
allocates no payload. Each reader retains its exact registration, and receives
its batch immediately after qualified publication, before another reader can
evict it. A batch waiting in a response channel still pins charged bytes.
One retained source-validation worker can use a reserved metadata allowance:
2 MiB scratch, 512 KiB source metadata and 16 KiB framing. Optional snapshots
and persistent native/source pins cannot spend it. The final actual worker or
attempt owner returns that allowance, not observer cancellation, a released root
lane or a published result. Concurrent workers and any excess must fit ordinary
metadata space. Native-input anchors have their own exclusive 1 MiB allowance,
held through the last actual proof/observation reference. Larger or concurrent
anchors fund their excess/full charge from ordinary space. ACK identities own
the separate 512 KiB ACK allowance through their last ticket/attempt/result,
even after publication; subsequent retained ACKs cannot reuse it prematurely.
None of these loans grants receive or canonical-commit authority.
Separate ACK and reader allowances remain reserved; inventory,
recovery indexes and registration-end intents remain independently charged.
This reduces admission space within the same 8 MiB ceiling rather than enlarging
it, and does not grant I/O authority or enable durable mode by default.
Stored cumulative ACKs use the original adoption-bound source digest, so an ACK
can settle cold sources without loading an entire prefix.

The original-source lifetime index uses weak allocation references, not replay
membership: native proofs and actual workers can retain a source after its
indexes disappear. Admission and trusted cold reconstruction populate this
index before exposing sources. It is bounded to 10,000 live allocations and
charges 256 logical metadata bytes per entry within the existing ceiling; dead
entries release their index nodes. A runtime/root-bound retirement fence records
then-live exclusions within a frozen allocator range. It proves neither an
accepted occurrence nor durable settlement (a cancelled sequence can be a gap),
and grants no disk deletion authority. Its first actual capture has a separate
164,096-byte precharged allowance (4,096 bytes plus 16 bytes per possible live
source), independent of worker, native and ACK allowances. Optional captures
cannot spend it; a concurrent fence pays its full ordinary charge. The final
actual owner returns it. This reserve reduces ordinary admission room within
the same 8 MiB ceiling and permits absence inspection under optional metadata
pressure, not a guarantee of complete reclamation progress.
Envelope-only delivery pins still charge
the payload cache independently. Physical reclamation, dependency closure and
crash-recoverable cleanup remain unfinished; this evidence does not enable them
or default durable activation.

Read-only original retirement inspection streams the same strict inventory
validators in one owned store snapshot, retaining only one source's recipient
slots rather than another full history index. Its lower-issued input charges
source decoder/scratch/framing and a 256 KiB slot accumulator before I/O; that
extra accumulator still needs ordinary room. It reports durable unsettled slots,
shared cumulative ACK dependencies and native input/handoff references. Expiry
exclusions alone do not release terminal outcomes. These observations neither
authorize deletion nor dispose of canonical evidence, and do not implement a
closed resumable cleanup plan or a full at-cap reclamation guarantee.
The read-only shared-ACK candidate analysis follows transitive ACK references in
the same snapshot, bounded to 64 originals and 64 strict scans. It refuses live
dependencies or larger closures without returning a partial candidate. Its
result retains the charged original input; candidate metadata shares the existing
256 KiB accumulator. Unsettled slots and native journals remain blockers. This
does not install a runtime cleanup lane, resolve native dependency closure or
authorize an intent, deletion, rewrite or default durable activation.

Storage-policy sources preserve immutable schema2 native attribution. Guarded sources retain schema3 recipient attribution; clock-qualified sources use schema4, additionally recording the original store allocation as a descriptive elapsed domain. Each source records one original registration namespace per recipient without a reconnect credential or live end marker. Legacy sources remain unknown for this added attribution. Each source records the
original native sender/recipient conversation and opaque incarnation (or explicit
raw/unbound absence) in the original recipient order. The actual native shell,
layout and exact registration supply these facts; matching pane IDs and sequence
floors do not. Duplicate receipt lookup also requires the original native sender
incarnation, so an independent binding cannot claim an older stored acceptance.
Exact rollback preserves its own receipt without a new write. Independent resume
creates a fresh shell incarnation even for the
same conversation; exact transaction rollback preserves the original. Temporary
ephemeral work suspends delivery to its qualified durable parent without retiring
its mail or stopping new pending-mail acceptance. A first-observed ephemeral
binding without a qualified parent remains unavailable until its actual parent
returns; stale association data, transcript seeds and repeated refresh cannot
supply that qualification.
Per-occurrence ACKs remain authoritative across binding changes. Exact rollback
can rebase a cursor hint to reveal earlier unread own mail; committed slots stay
excluded. A late ACK for another incarnation settles its original slots without
advancing the restored shell's cursor. ACK reservations include this original
owner in their bounded metadata.

Metadata is reserved before audience copies, included in the source checksum,
and checked on reload. Native conversation attribution is bounded to 256 UTF-8
bytes, and its incarnation uses a complete 64-character SHA256 identity.

Decoded attribution is quarantined evidence. It cannot recreate a live native
binding or authenticate reconnect, canonical context commit or provider work.
Schema1 sources keep legacy-unknown ownership; no missing evidence is inferred
from current discovery. Generic schema9 capture refuses source state containing
schema2 ownership/classification. The host checkpoint path now captures this
state as schema10 cold-reference format1: original sequence/SHA256 references,
receipt and original TTL basis, recipient delivery slots, allocator floor, and native registration
namespace/shell incarnation tuples. Bodies remain in the original store. Current
raw discovery and connection credentials are excluded. A genuine duplicate lookup
after TTL expiry may retain an `expired` receipt; checkpoints preserve that status
and its original finite TTL rather than reject the retained original.

Active pane/conversation metadata rows also preserve the actual shell incarnation
in version8. Versions1 through7 retain unknown native identity during migration;
no default value is invented. The saved UUID bytes are descriptive, and generic
transcript restore keeps creating a fresh live incarnation. This persistence
prerequisite does not enable original-store recovery or native delivery dedup.

An actual root-owned blocking read verifies every original source in one SQLite
snapshot and binds the checkpoint to the original store allocation, owner,
generation/watermark and directory/database identities. Verification and encoding
copies share the finite metadata budget through publication; capacity refusal
preserves accepted mail. Cancelled observers do not suppress requested writes,
and actual worker loss retains the exact capture for joined, backed-off retry.
Generic restore rejects schema10. These descriptive facts do not yet provide
trusted whole-session cold reconstruction, context-commit-before-ACK dedup or
native transport/disposition settlement; those remain activation gates.

The fenced storage recovery reader now checks cold references together with the
complete inventory and ACK history in one SQLite snapshot. It preserves later
accepted originals and lost positive ACK results as unpublished observations,
rejects copied stores and contradictory source/ACK facts, and uses the persistent
watermark for the allocator floor even after row removal. Input, inventory, fold
and decoder scratch share the metadata budget; bodies are decoded one at a time.
Snapshot terminal bits without durable journals refuse qualification. This reader
does not yet install a recovered root/session or restore native authority.

An internal expiry journal boundary now freezes up to 64 original slots from
one accepted source. It preserves the first strict TTL observation through
uncertain writes without publishing outcomes before actual commit/adoption.
Fresh receive remains excluded through clock rollback, while genuinely offered
unselected fanout slots retain their ACK rights. Original source metadata and
bounded reader scratch remain accounted through their actual owners. Expiry,
recipient retirement, horizon release and sender consumption stream original
payload validation without reserving a second payload-cache allocation. A
retained delivery filling the cache therefore cannot block those dispositions;
actual worker/nonce/source metadata still survives cancellation, loss and retry.
Actual reload/cold construction retains its separate full-body reservation.
Canonical handoff reserves metadata decoding/recovery scratch through actual
worker join and retry; its source validation streams without a second cache body.
A pinned delivery filling the cache cannot block original canonical handoff or
its later sender consumption.

The concrete fenced SQLite port writes bodyless `t1` records of at most 340 bytes,
bound to the original allocation, source fingerprint, first clock and exact
slots. It validates original TTL, excludes conflicting ACK/native facts and
rolls back unrelated-row mutation. Exact bytes reconcile a lost commit reply.
Inventory and cold reconstruction restore actual unreleased observations even
when the commit's adoption was lost or the checkpoint predates it; saved release
bits cannot substitute for journal evidence. Physical tests cover close/reopen,
commit-reply loss, trigger rollback, allocation tampering and independent fanout
ACKs. Additional subprocess controls now exit after actual t1 or r1 commit
before live adoption and recover through the real original factory without a
manual message checkpoint.

The internal actor now owns expiry selection and its original root continuation.
The exclusive root retains the actual worker, first plan and lower escrow through
cancelled observers, joined worker loss, backed-off retry and shutdown. A failed
post-commit physical position read retains the positive while only that read is
refreshed. A strict original TTL deadline wakes an idle actor and reopens native
probes first; unresolved canonical input or application work withholds selection.
Pre-issuance capacity refusal backs off while unrelated work can use the lane.
Connected controls exercise real post-commit worker loss, physical root relocation,
position-only retry, kernel-owner retirement and an otherwise idle actor whose
shutdown waits for expiry adoption. A separate subprocess exits with actual
canonical preparation; startup after the original TTL expires still completes
that canonical input and its exact ACK before any terminal selection.

The same source-owned terminal worker now journals finite outcome-horizon
release. It freezes up to 64 actual unreleased terminal before-images and one
release observation; actual r1 commit and original adoption precede live release.
Version1 r1 records fit 916 bytes and bind original allocation/source/slots/first
clocks. Each release must match actual terminal evidence and its 600000ms threshold in the original clock domain
in the same snapshot. Conflicts, missing observations, duplicate releases or
unrelated-row damage refuse. Lost replies reconcile the same first bytes/plan.

Cold recovery reconstructs release bits and pending custody from actual r1,
even when adoption or a newer checkpoint was lost. An actual observed source
horizon remains usable for later bounded batches through rollback and restart;
generic snapshot bits cannot create it. The actor wakes for expiry or horizon,
retaining issued work through shutdown. All recipient slots must ACK or release
before logical pending custody ends; physical reclamation is separate. These
journals do not consume a sender outcome or grant another owner its handoff.

Guarded volatile cleanup and direct sender consumption remain withheld. A
volatile runtime hint alone is not durable canonical handoff proof.
The internal handoff capability now freezes the actual original native sender
and unreleased terminal before-image. Live capture checks the original sender;
historical lookup retains attribution without recreating a task or provider.
Slot protection and metadata remain owned through the last anchor/proof clone.
A distinct typed terminal-handoff codec preserves the fixed neutral RuntimeHint
and rejects altered semantics, provenance or chronology. Replay restores only
that neutral reference. Shared physical transcript custody now supports separate
v4 handoff images while preserving native v3 bytes: both classes retain actual
file/prefix/row ownership, fence ordinary mutations and survive generic recovery.
The internal original-root worker now also commits bodyless u1 preparation and
h1 canonical journals, each bounded to 4096 bytes. They bind the original source,
first terminal before-image and physical append plan without duplicating IDs or
payloads. Source/t1/ACK conflicts are checked in one snapshot using a reserved
decoder and scratch owner. Transactions protect non-target rows and reconcile
the same first plan across failure. Actual h1 commit precedes canonical receipt
retirement and opaque proof; it leaves the terminal outcome pending. Cold lookup
can finish u1 or retire an orphan h1 image without a live sender, task or another
archive position. Historical proof after legitimate archive cleanup grants no
fresh target or archive observation. The root retains actual positives and I/O
ownership through lost replies, retry and Stop. These internal journal/root
boundaries remain internal until all durable lifecycle and ingress gates pass.
The internal terminal lane now also supports separate canonical-proof-only s1
consumption. Its 157-byte frame binds the original first terminal and source to
the actual h1 row/digest. Commit and original adoption precede release; neither
changes ACKs, cursors, first clocks or another recipient's horizon. Cold recovery
distinguishes consumed slots from actual r1 horizon observations and checks the
original h1/source/t1/ACK evidence. Issued consumption can drain through Stop and
exact retry.

The internal actor now probes terminal slots before horizon release. Historical
positives settle only original custody; they never reconstruct an action or task.
After qualified absence, the runtime checks its actual accepted-action owner,
turn incarnation and conversation before preflighting one neutral context event
and canonical prefix. Earlier ordinary producers, native input and active
inference defer preflight. The original projection and target survive canonical
write, Stop and consumption. Provider, peer and restore gates wait; ordinary
bookkeeping cannot race an issued canonical writer. Actual proof attaches before
fallible frontier application, and local backoff retains it through application
or consumption-admission failure. Only actual S1 adoption removes that owner.
Guarded timers no longer append a volatile fact and attempt direct consumption.
A parked lookup's expired timer waits while another retained handoff owns
application/consumption backoff; its original source resumes after settlement.
The current work's own pre-issuance retry remains wakeable. Native input
admission timers also wait while that handoff excludes their pump, avoiding
immediate ready loops over an unavailable continuation.
These facts never settle waits, change accepted results or create provider work.
Native action sends remain TTL-free, so their normal terminal path still needs
validated owner retirement. Focused expiry controls set a finite trusted TTL
before genuine action acceptance; they do not establish retirement qualification.
Fresh registered custody now uses the original root's suspend-inclusive elapsed
clock for acceptance, duplicate status, offers/native selection, expiry,
retirement, horizon release and deadline scheduling. Immutable source4 stores
its original clock-domain allocation; decoded bytes do not install a reader or
restore a native owner. Reload budgets and checks that domain before discarding
the source wrapper. Generic capture refuses clocked rows rather than lose timing.

SQLite schema3 adds one fixed, checksummed 116-byte clock blob in the existing
state row, charged 180 bytes including framing. Its progress commits in the same
transaction as custody. Exact retries do not write a newer clock. Worker loss
retains the actual original reader, including native failure; fresh timing
operations refuse failure while already-issued immutable work may settle using
its actual last-good observations. No fresh epoch rescues that same root.

Same-boot recovery includes suspend and daemon downtime. Across a genuinely new
native boot, the root preserves last committed elapsed progress and adds the new
boot's uptime. Unknown old-boot time since the last custody commit and powered-off
time are excluded; an uncommitted clock observation cannot preserve that interval.
This policy never guesses UTC duration, resets original acceptance or terminal
timestamps, or treats reboot as native retirement. Original TTL and horizon
windows keep their accrued committed progress. Schema2 migration preserves all
rows with explicit unknown/null legacy timing; nonempty legacy stores cannot
silently acquire a fresh elapsed epoch. Unqualified internal legacy paths retain
their wall policy without an elapsed guarantee.

Validated retirement and elapsed clocks have internal implementations;
reclamation remains unfinished. These internal operation bounds
do not establish complete admitted-state capacity or enable normal host/all-
ingress activation.

Inventory and checkpoint/ACK, terminal, native-input and handoff proof validation
now stream the original payload and retain its decoded length/SHA256 instead of
allocating a body. The descriptor shares full decoding's JSON/UTF-8/escape,
resource and semantic checks; the store still verifies the complete original
encoded checksum and framing. It preserves frozen recipients, namespace/native
attribution, TTL and clock domain. The descriptor cannot become an empty message,
reload result or delivery. Actual reload and cold construction use their
separately reserved full-body reader. No source or receipt is deleted by this
metadata-validation path; evidence-preserving reclamation remains a separate
required step.

The trusted storage-only reconstruction path now moves those originals into the
canonical message owner as cold metadata, preserving pending obligations,
source-bound ACK slots, native attribution, receipts, TTLs and allocator floor.
One reserved payload quantum covers inventory and source decoding; no body is
retained until an explicit original-root hydration. Input/history and incremental
recipient counters share the metadata budget, and any refused row withholds the
whole candidate. Endpoint registrations remain unavailable: session/native
continuity installation is still required before publication.
Genuine ACK offer clocks survive endpoint clock rollback, including an offered
zero before acceptance; recovery retains the original TTL upper boundary without
inventing an acceptance-time minimum offer clock.
Narrowed cache limits apply to initial inventory and ACK-source decoding as well
as row reconstruction. Oversized originals are refused before their bodies are
decoded, preserving the original store for a retry with sufficient capacity.

Recovery now runs on an owned blocking worker with retained immutable input,
actual join observation and monotonic retry. Cancellation cannot replace it or
publish partial custody; retry keeps the first observed original store position.
Opening requires the existing private directory, owner lock and current-schema
database, and never creates or initializes a missing/blank replacement. Last-root
retirement drains the actual worker before releasing its kernel fence.

The joined startup candidate retains its root and checkpoint metadata reservation
through cancellable setup, preserving native facts for qualification.
Storage-only recovery leaves endpoints absent. The dedicated factory below
qualifies private native contexts before registration and publication.

The original root also has an owned native registration journal independent of
manual message snapshots. Its bounded version2 `n1` observations preserve the
actual namespace, shell epoch, native owner, immutable identity and interval
floor under the original allocation. Recording that floor reserves cancelled
sequence gaps atomically without creating messages or acknowledgements. An
outstanding acceptance or ACK reservation must settle before capture. Records
are limited to 240 KiB encoded and 512 KiB conservative decoded metadata.

Cancelled observers and lost positive replies retain the original input and
first preimage for joined, backed-off retry. Only exact adoption marks a current
same-runtime binding as recorded. Replacement namespaces or shell epochs cannot
inherit the marker. Immutable identity reconciliation can replace a namespace
without changing shell epoch, CID or floor, so saved labels alone remain
insufficient. Inventory validates each fact and retains only its compact key.
Stored observations alone do not restore native authority or prove current
lifecycle availability. The dedicated factory below additionally qualifies the
actual private session and context.
Journal transactions also preserve a fingerprint of every non-target row from
the original input. Trigger deletion or rechecksummed mutation of an unrelated
source, ACK or native fact rolls the whole attempted write back.

Guarded n1 transactions now retain immutable version1 `b1` lifetime provenance
for their actual current and previous guarded tuples. Each record is at most
4096 bytes and contains the original allocation, namespace, shell epoch, derived
owner, agent/floor and first known guarded publication. Availability is normalized
false; capability vectors are omitted. These descriptive facts supply neither
retirement nor live restore authority. An exact original parent can return after
binding replacement, so a changed or missing latest n1 cannot prove its end.

The same original transaction freezes at most three records and its first input,
history preimages and non-target image, with exact target checks after SQL effects.
Lost replies reconcile that batch. Equal history retains its first known
publication; backfill uses only an actual present guarded prior n1 and never
invents missing/overwritten legacy history. Inventory validates bounded history
keys and cross-checks matching current observations. Native capture now reserves
2 MiB of recovery scratch and bounded plan metadata through actual join and last
input ownership. Linked retired-recipient observations remain separate from
the explicit registration-end journal.

Guarded native publication now pre-reserves a bounded intent retaining the actual
registration allocation and original floor. Explicit removal and immutable
authority replacement freeze its first last-allocated cutoff without allocating
after removal; shell displacement and objective/presence refresh do not end it.
Repeated captures share one charge of 4096 bytes plus bounded agent attribution;
qualified cold bindings reserve the same lifecycle owner.

The actor drains actual ended allocations through immutable version1
`o1:<namespace>` records on the original root, including during Stop. Each record
is at most 4096 bytes and binds the original allocation, generation, namespace,
agent, floor and end cutoff. The first input/position and all unrelated rows stay
fixed through worker loss; retries require actual join/backoff and exact target
checks. The cutoff reserves allocator gaps atomically, without accepting mail.
Worker scratch and encoding metadata stay charged through original adoption.
Capture refusal backs off without issuing I/O or spinning. A foreign service
cannot consume the original positive.

Inventory cross-checks related native facts against ends. An ended namespace
cannot publish a late n1 or return through an older optional checkpoint and
matching private context. Live restore preflight refuses already-pending end intents. An o1 record
itself changes no message slot, ACK or terminal outcome.

The internal native lookup checks canonical receipts and recovers original append
preparation before considering that end. Actual absence plus exact b1/o1
namespace, floor, cutoff, publication order and fingerprints can return an opaque
source-bound witness. Its lookup anchor reserves 32 KiB structural metadata.
Missing provenance remains unknown. Existing application input ownership,
canonical commit, ACK or known source expiry wins before retirement selection.
The common terminal worker journals one original slot as a bodyless 168-byte
version1 `d1` record, preserving its first source, clock and unrelated row image
through worker loss and joined retry. Only original adoption publishes
`recipient_retired`; no ACK, peer input, task or provider continuation is created.
R1 horizon release, canonical sender H1/S1 and cold folding preserve that reason.
Metadata pressure uses bounded lookup backoff.

Raw source retirement uses schema3's exact allocation and its genuine one-way
end marker. An equal-ID/floor replacement cannot inherit that source. Raw
selection waits for actual o1 adoption, so terminal I/O cannot block its own
prerequisite. A 168-byte version2 d1 binds explicit raw classification, source,
slot, namespace and the original end snapshot fingerprint; it carries no native
lifetime digest or canonical-input claim. New guarded source metadata must fit
the actual 512 KiB decoder before sequence reservation/I/O, so admission may
conservatively stop below the independent count ceilings. Failed raw source
reservations release transient end-intent charges; native publication and actual
ended work retain their own obligations. The same R1/H1/S1 and cold history
preserve `recipient_retired` without fabricating an ACK.

The one-use qualified cold factory restores no raw reconnect owner. It retains
end intents for explicit old raw namespaces, preserving any original o1 cutoff
and otherwise bounding the end by the persistent allocator. A namespace actually
restored as native is not ended by raw absence. Generic snapshots/UUID bytes
cannot select this path. Shutdown retires actual raw leases after original ACK
settlement, then drains their end journals before releasing its reply/root.
Fresh terminal work stays withheld during Stop.

Actor-owned `snapshot/resume` now stages and validates layout/message metadata
and runs resume hooks before retiring native pane identities. One original
request waits for issued writes, canonical proofs/application results and old
ends, then journals the ends created by native retirement on the same root
before layout application. Fresh native input/probes/publication and further
message/turn admission pause during that drain; existing original results still
settle. A concurrent resume receives a conflict. Cancelling the reply does not
cancel issued original storage work.

Generic snapshots validate their message bytes but retain an attached durable
ledger, allocator, scope and root, including messages accepted after the
snapshot. Actual same-daemon raw connections survive live layout restore and
are never classified unavailable from generic snapshot absence.

With an internally registered custody actor, terminal `load-layout` uses the
same snapshot drain from textual/control commands, key bindings, the colon
prompt and `terminal/step`. Its parsed command sequence and original reply stay
owned while prefix receipts and selected status/layout I/O finish. Later
commands and buffered attached actions run in order after restore; old layout
coordinates cancel as stale. Failures stop later commands and propagate instead
of returning a successful Noop. RPC keys stay pending until the final response
is cached, and reply cancellation cannot detach an issued original. A step RPC
keeps its consumed/application/view shape and refreshes the current view with
empty input, without repeating input or resize. Deferred diagnostics publish
actual outcomes. Clock/reboot, reclamation, complete resource/ingress and final
whole-issue gates still precede normal durable activation.

Internal actor spill setup now enables a trusted native admission barrier. The
actor journals current native intervals before selecting queued authorized
actions, whose original source owners retain their bodies. New reservations and
storage-qualified duplicate replies wait while any current native interval is
unrecorded; native input also waits for its exact interval. Direct ingress can
receive an explicit never-accepted busy result and retry after journal progress.
No sequence or native request generation is assigned for that refusal.
Enabling the guard invalidates earlier unguarded live markers. A late unguarded
completion cannot open guarded admission, even for the same binding; repeating
setup preserves already recorded guarded markers.
Existing genuine ACKs remain settleable, including when a metadata capture was
refused before issuing I/O. Actual issued workers retain their actor/root owner
through loss, backed-off retry and shutdown. Replacements require their own
journal adoption. Qualified internal session startup retains the same barrier.

A dedicated internal SessionFactory path now qualifies original custody
and native observations in the same SQLite snapshot, then registers the actual
root with the actor before publication. Version2 journal records include their
commit generation and the actual admission guard at capture. Legacy version1 or
unguarded observations cannot restore a native endpoint. Unknown legacy source
ownership also remains unavailable to a recovered native binding.

Schema10 checkpoints keep exact source-slot ACKs independently of the cumulative
cursor. A later native ACK above cursor zero does not settle an earlier pending
original. The earlier actual commit/ACK can then advance through both explicit
positives. Factory tests exercise this with real canonical and ACK journal writes
before dropping the original owners. They also replace the actual private epoch
at the same CID before ACK: the old positive stays with its original slot,
leaving the replacement cursor zero and creating no replacement task/provider
work. Saved positive bits cannot replace missing original ACK journal evidence;
factory publication refuses while the accepted originals and canonical row stay.

This path reads the existing private active-session checkpoint and actual
transcripts, preserving the checkpoint through qualification. Original epoch,
CID, pane, window, project and native harness must agree. Opaque qualified epochs
install only into the exact provisional runtime that supplied that evidence;
a second runtime with equal labels cannot adopt them. Raw registrations remain
absent. Unix listeners and pane processes start only after qualification, and
startup retains the original root and metadata leases throughout installation.
Final checkpoint publication compares the original under the compound writer's
session lock, preserving any newer checkpoint on refusal.

Fresh internal factory setup now publishes a private original registration before
returning its actor. The fixed MMPROOT1/registration.v1 record binds original
UUID, owner and actual directory/database identities, independently of manual
message snapshots. Its checksummed framing fits 362 bytes; exclusive staging,
file sync, no-overwrite publication, exact stage cleanup and directory sync own
at most two such files. Nonempty history cannot be retroactively registered.
Foreign/malformed publications or an unowned staging collision refuse startup.

Without a message snapshot, restart builds an empty-reference schema10 seed
from that existing original registration. The same complete inventory/source/
ACK scanner reconstructs actual originals, settled slots and allocator gaps.
The first seed/position remains owned through retry; no missing original is
initialized. Copies, changed allocations, unsafe files and unknown framing
refuse. Generic factory startup rejects an existing custody root before socket
or process publication, even when the layout lacks message state.

Fresh setup also initializes an empty scoped private checkpoint, preserving any
existing conversation bindings on refusal. This entry still needs trusted
whole-session layout input on restart and a scoped active checkpoint no larger
than 256 KiB. Changed, missing, corrupt or oversized private evidence refuses
startup. Generic message restore still refuses schema10 and creates fresh native
epochs. Complete native canonical commit recovery, terminal journals, reclaim,
complete admitted-state resource/liveness coverage and normal host/all-ingress
activation remain unfinished. No public option is added.

The internal root worker also has a canonical native-input storage boundary.
It retains one actual source, original transcript target and first append plan
through cancellation or a lost worker reply. A bodyless `p1` journal freezes
physical destination identities, prefix and exact row position before writing
input. The conversation mutation lease spans both journals and the canonical
append. After the exact row is read and synced, one transaction replaces `p1`
with bodyless `c1` evidence. Each journal fits 32 KiB; non-target custody rows must
survive every SQL effect. A changed or missing original backing refuses retry.
Retained source metadata remains charged through the final proof owner, even
after transport ACK and replay eviction.

The existing transcript receipt pipeline also retains a version3 canonical row
image ending in the native input, with its preceding new chronology and original
batch/final-row physical plans. Projection and bounded size admission happen
before live context/task commit. The original transcript lease deduplicates
already committed event identities and freezes the exact batch. The receipt
precedes prefix writes, prefix rows precede `p1`, and the native row follows `p1`.
Ordinary writers wait for this owner; generic transcript recovery validates it
without consuming it. Original-root recovery can finish its missing or partial
row without loading another envelope or creating a task. Only `c1` permits the
exact receipt to retire. Each internal operation allows 2,048 rows / 32 MiB of
encoded canonical rows, with a 64 MiB plus 256 KiB receipt file bound; complete
admitted-state resource/liveness qualification remains outstanding.

The original attempt retains its actual conversation lock and physical handles
even when receipt publication fails before creating a file. Retry keeps the
first batch position; a competing ordinary writer cannot take it. Native
issuance waits for queued and in-flight ordinary transcript rows. After actual
issuance, new bookkeeping reads for that conversation wait for native commit,
including Stop bookkeeping, so they observe the committed frontier.

Original-root lookup checks unpublished native staging under that same lock.
A complete image must match its accepted source, original preparation if present,
and physical batch/suffix before the same file is promoted. A final/temp pair
reconciles only for two names of the same object. Corrupt complete images,
conflicting owners and replacement backing refuse; append, fork, delete and
prefix mutations also fence staging. An incomplete private JSON prefix can be
aborted only by lookup without a live projection and with neither journal.
Final publication precedes canonical bytes, so this abort commits no input and
preserves the accepted envelope. Genuine selection can then create a new local
message turn; it does not restart the interrupted task. An existing live attempt
instead finishes its original partial bytes and selected position.

Sequenced ordinary peer projections use a digest-validated `peer_reference`
context event for up to 2 MiB of exact untrusted content, including NUL bytes.
These references have no native attribution or ACK rights. Legacy ordinary
prompt-boundary frames keep their smaller decoding contract.

The archive row is typed untrusted peer-reference data. Its provenance does not
become delivery authority through deserialization, and later bookkeeping treats
its ordinary chronology projection as the same event. Only actual root storage
qualification can seal a native commit proof for the exact frozen source slot.
An existing `c1` can settle that historical source after context replacement
without injecting it into a successor. Root-boundary tests cover these actual
writes and ACKs. The guarded internal actor probes the original root before live
receive or unread-prompt injection, retains each selected canonical projection
through storage and ACK, and holds its provider/scheduler/peer continuations.
The actor retains each joined result before application handoff, and its receive
receipt owns sealed proof before position/request-reply/coordination updates.
Recoverable application errors use per-receipt monotonic backoff, independent
of physical storage retry, while unrelated ingress and other application owners
remain eligible. Historical `c1` scans cannot ACK around a live receipt's failed
application step. ACK admission refusal keeps its original proof. After exact
ACK, an unfinished original scheduler handoff remains in that receipt through
presentation, pane, conversation and terminal cleanup; shutdown suppresses new
provider continuation. Local retry and saved display data grant no new task
ownership. Position/provider updates also require the original actual native
namespace: reusing the same conversation ID with a fresh private incarnation
cannot inherit the old input's counters or provider offer. Read-only owner
observation after ACK does not reopen delivery eligibility.
Interrupted bookkeeping excludes that root-owned peer occurrence. Shutdown
drains selected input without opening a provider continuation; a successor
context cannot inherit an old completion. Cold-read capacity refusal retains
one payload-free source with a monotonic retry deadline while other actor work
continues. A delivery or eligibility notification received behind an active scan
cursor preserves one further pass, so earlier cold sources are revisited. The
scan stops when exhausted without another notification.

Process-exit tests stop after canonical commit, after preparation while the
native row is still missing, and during partial/complete unpublished staging.
For a complete staged image, the real factory and actor qualify the original
binding, finish or deduplicate the canonical row, and ACK its historical slot
without creating a task or provider request. Partial staging is aborted before
genuine fresh selection, preserving the interrupted task unchanged.
A real synced archive observation
may advance only the current original private transcript frontier; descriptive
serialization discards that observation, and historical `c1` alone cannot claim
current backing.
Historical lookup itself does not add a subscription; qualified startup may
already have restored one for the original native binding. Built-in native continuity does not depend on an optional discovery
capability label; the original guarded binding and private context must match,
and explicit external harnesses remain excluded. A current proven native binding
also preserves an originally absent project scope when its restored process has
a working directory: receiving mail must not replace that registration through
metadata enrichment. Terminal and clock journals remain subject to whole-issue
qualification; reclamation, resource/liveness and normal all-ingress activation
still require implementation and qualification.

Fresh opt-in requires an actually empty original store; only qualified recovery
can install an existing store. Normal product construction keeps durable mode
disabled: complete crash recovery/liveness, native restart deduplication and
all ingress remain separate integration gates. Existing stores and saved
snapshots cannot select this policy. No configuration schema or wire field changes.
Normal product traffic retains the existing resident limits.
Replay limits and payload validation remain independent; a pending byte budget
does not enlarge the maximum individually acceptable envelope.

The retained recipient set is bounded independently of payload bytes: at most
4096 records and 1 MiB logical metadata, charging 192 bytes plus each UTF-8 ID's
length. New sends reserve the whole fanout before sequence allocation. Capacity
rejection (`code: "capacity"`, `status: "rejected"`, no accepted receipt) leaves
the prior accepted set unchanged. Native send failures preserve the `capacity` code,
`delivery_status: "rejected"`, `accepted: false`, `delivery_applied: false` and
null message ID; no success receipt is assigned. Only fully settled history that the ordinary
replay count/byte policy actually evicts may release metadata for the new send.
Binding/offer/commit/terminal/sender metadata counts the deduplicated
replay/custody union.

Immutable auxiliary envelope metadata has a separate 16 MiB logical budget over
that union after ordinary replay eviction, narrowed by `auxiliary_bytes` in the
trusted construction policy. Each occurrence charges 1,024 bytes plus 64 bytes
and four times UTF-8 length for each fixed envelope/sender/selector string slot,
capability and extension key/value. Empty variable entries still count; payload
and recipient records keep their separate budgets. This includes index and
acceptance string copies. Over-cap acceptance rejects atomically; snapshot
canonical sources and acceptance projections must fit the destination policy.
Logical content accounting is independent of allocator capacity or RSS.

Both terminal outcome query forms return at most 100 records and 64 KiB logical
copied data, charging 128 bytes plus message-ID and recipient-ID lengths per
record. Each individual record must fit at acceptance. A query stops before
cloning the next over-budget result; exact consumption permits later batches to
progress without payload disclosure or changed ownership.
The socket write-versus-application-commit limitation below remains unchanged.

`recipient` is an object with exactly one supported selector: `agent_id`,
`pane_id`, `window_id`, `role`, `capability`, `group`, or `session: true`.
Multiple independent selectors are rejected. A selector such as
`{"session":true}` or `{"group":"session"}` does not itself widen audience;
with omitted scope it still selects only the sender's project peers.
For `agent_id`, use the exact nonblank, control-free ID returned by discovery.
Generated IDs such as `a1` and opaque native IDs such as `agent-%1` are both
direct addresses. The parser does not trim or reinterpret them as pane IDs,
roles or display labels, and addressing an ID never registers it. Exact current
registration, availability and audience checks still apply; missing, retired
and audience-ineligible targets remain indistinguishably unavailable. Pane/window
selector grammar, sender binding and framing limits are unchanged.

For a raw client registered by `hello`, project discovery shows only self
when no additional discovery filters exclude self, until the trusted runtime
supplies membership. Default/project sends fail
without that membership. Explicit `scope: "session"` allows session-wide
discovery or delivery within the local service. For example, after `hello`,
substitute its assigned agent ID in this complete request:

```json
{"protocol":"mmp/1","id":"docs-handoff-1","type":"send","time":"2026-01-01T00:00:00Z","sender":{"agent_id":"a1"},"recipient":{"role":"worker"},"scope":"session","correlation_id":null,"ttl_ms":null,"content_type":"text/plain; charset=utf-8","payload":"Protocol documentation is ready for integration."}
```

The caller's own `sender.agent_id` must match the registered connection; the
service uses the canonical registered identity, not sender-supplied membership
or objective claims. Both nullable `correlation_id` and `ttl_ms` are required
in these full raw envelopes even when null. Discovery uses a reduced request,
for example `{"protocol":"mmp/1","type":"discover","scope":"session"}`;
its result reports the resolved `scope` and filtered `agents` identities.

## Delivery, expiry, and errors

MMP preserves acceptance order for one sender, one recipient, and one logical
channel. Explicit receive returns a `deliver` object shaped as
`{"protocol":"mmp/1","type":"deliver","cursor":{"recipient":"...","last_sequence":N},"messages":[{"sequence":N,"envelope":{...}}]}`.
An `ack` advances the subscription cursor only through an occurrence selected
for that exact acceptance-time recipient registration. Every earlier retained
own occurrence above the committed cursor must also have been offered, unless
expired at the endpoint's observed offer time. Foreign global sequence gaps
are harmless; unoffered, foreign and future ACKs reject without mutation, and
duplicate/regressive ACKs are inert. Explicit sequenced receive and automatic
fanout share this evidence; historical and unsequenced reads do not authorize
ACKs. Native context commit still precedes acknowledgement.

The current automatic fanout path, however, advances its server-side
cursor after writing a delivery frame rather than after a recipient `ack`.
This is a known implementation conformance gap against the normative
at-least-once requirement in `SPEC.md`: automatic fanout is currently
connection-oriented best effort. A disconnect after the server write but
before application consumption can lose that unconsumed delivery. Integrators
must not treat the current automatic fanout path as an end-to-end receipt.

The sender receives an `ack` with `message_id`, `queued_recipients`, and
`status` when a message is accepted for delivery. A recipient's cursor-advance
`ack` instead supplies `sequence` (or `last_sequence`) and receives the new
`last_sequence`. Body-level failures use
`{"protocol":"mmp/1","type":"error","error":{"code":"...","message":"...","retryable":false,"delivery_status":"..."}}`.
Current dispatch can emit `unsupported_protocol`, `payload_too_large`,
`expired`, `invalid_envelope`, `not_found`, `unauthorized`, `undeliverable`, and
`internal_error`, depending on the underlying failure. Framing failures such as
malformed or oversized frames can terminate the framed request before an MMP
error body exists.

Malformed framing (including invalid UTF-8 or content type), oversized input,
EOF/reset and response or fanout socket failures close only the offending
connection. The same listener and runtime actor continue serving replacements;
other panes and agents are unaffected. Framing is not resynchronized and no
request or delivery write is replayed. Body-level protocol rejection still uses
the error response above when a valid frame can be decoded.

Raw `hello` allocates a fresh observational identity, not a binding to an
existing pane identity. Repeating `hello` retires the previous raw identity;
connection exit/cancellation retires its latest identity and subscription.
Stale cleanup cannot retire another socket's fresh identity. Shared retained
envelopes and acceptance receipts remain intact, and a failed fanout write does
not advance its cursor. Continuous reaping releases active slots after cleanup;
final drain uses the same containment rules. Diagnostics log fixed reason
categories and aggregate counts at power-of-two failures, not peer data.

Actor ingress, audit/shared-state and listener accept failures still escalate;
task JoinErrors retain the existing critical policy. This isolation is based on
typed operation provenance, not blanket suppression of I/O or state errors.
Socket ownership is published before the hello reply, including when an audit
error follows registration. Final actor cleanup owns any raw lease still live
at shutdown; independently registered pane-less identities remain untouched.
Cleanup ownership corruption is critical and reaches completion after pane
termination, rather than being reported as a successful peer closure.

Private MMP snapshot schema9 retains schema4's distinction of fresh raw sender provenance from
reusable delivery floors. It preserves the exact objective versions in retained
authored mail, including when the current objective changed again without a
send. Every version must be referenced by canonical mail and agree on immutable
agent/project/pane/window/role/capabilities; mail, receipts and audience are not
rewritten. Releasing the final replay/custody reference removes unreferenced
versions, then the empty raw floor. This bounds version count by canonical authored occurrences, not by the
payload-only byte budget. Schemas1–3 reject the fresh-version field even when
empty. Current registration, recipient intervals and live proof checks remain separate
from historical source metadata. This does not provide raw reconnect authority.
Snapshot restoration also reserves exact recipient binding IDs still referenced by
retained mail, including raw non-senders and senders whose authored floor has
already evicted. A fresh generated owner cannot acquire that recipient's old
direct or fanout mail through historical reads. This uses existing bounded mail rather
than restoring idle registrations or keeping extra identity tombstones.

Schema9 adds a complete, ordered, metadata-only sender-provenance table for
canonical replay and custody sources. Each entry preserves a fixed-size, versioned digest of the exact original
sender, plus its interval and fresh-retirement classification. The full identity
stays in the canonical envelope; the table does not duplicate its strings. Re-registering an address,
refreshing an objective or changing current discovery metadata cannot rewrite
older authored mail. Snapshot capture omits retirement floors for active
registrations; their exact active registration interval still excludes old mail.
The table is cross-checked against canonical envelopes and receipts and stays
within their count/metadata reservations. It does not create live acceptance,
recipient rights, socket credentials or restarted task authority.

Valid legacy records are checked before being captured in schema9. A schema5–8
active identity with an exact redundant old retirement floor can omit that floor
when its registration high-water proves the old interval ended. Unknown or
conflicting legacy provenance remains an error; input snapshots are preserved.
Missing, extra, reordered or contradictory schema9 provenance and future schemas
are rejected. This remains snapshot recovery, not acceptance-atomic disk storage.

Schema5 records each active registration's acceptance sequence floor and each
retained occurrence's exact accepted recipient intervals. A floor is the last
allocated occurrence before registration, including unpublished reservations; all mail for an earlier registration is
at or below a replacement's floor. Capture/restore preserves these facts, not
arbitrary reconnect authority. Schemas2–4 with retained mail are explicitly
refused as unknown acceptance-time recipients; the service does not guess a
recipient set from current registrations, nor rewrite the input snapshot.
Empty-mail legacy snapshots still restore, while schema1 retains its existing
audience-less-mail discard policy. This format version does not promise an
acceptance-atomic disk write or transparent workflow restart.

The lower storage-qualified admission boundary separates reservation, issuance,
physical completion and actor publication. Its version1 immutable record streams
borrowed original fields, payload and exact eligible recipient intervals; the
encoded data is not a live receipt or capability. An unpublished occurrence is
absent from delivery, while existing accepted mail can still drain. Allocator
reservation gaps are not successful sends, and tail subscriptions do not skip
an original that is published later.

The lower tracked-request owner can reserve the original conversation/turn and
future handoff capacity before issuing storage work. This unpublished ownership
is not an accepted receipt. Conversation retirement and TTL maintenance cannot
release an issued request: its matching completion must still settle against the
original caller. Qualified noncommit releases both request and delivery charges;
positive completion publishes one receipt, and duplicate completion never sends
again. Product lifecycle integration must explicitly settle this retained work.

The private storage adapter encodes a complete acceptance record in64KiB chunks,
with a version1 stage header and final commit marker binding its full size,
chunk count and SHA256. It preflights all encoded rows and framing before writing;
there is no new256KiB payload limit. The original store allocation, occurrence and
starting generation form one retained plan. Partial writes remain unpublished;
retry validates the actual dense prefix and bytes rather than repairing corrupt
committed custody. This serialized plan must settle before later store work.
Chunk rows count against the current private store row/encoded-byte limits; the
full product envelope/obligation/cache and total physical quotas remain separate
integration work. Current runtime acceptance remains in memory.

The async storage worker component retains actual opening/write handles and the
original plan outside abortable observers. Tracked starts require the actual
prepared request owner; positive completion remains held until paired publication.
Unknown results keep the linear input, while lost results require the exact actual
worker join before reconciliation. Retry spacing is monotonic1s,2s,4s up to30s,
with one worker per root and no provider slot. Stop closes new issuance and leaves
issued work available for settlement. The supervising actor must retain this root;
fallback last-root draining is not canonical acknowledgement. Ingress/supervisor
wiring, later ACK/control scheduling, genuine storage
change wakeups, full quotas and crash/restart recovery remain integration work.

An explicit failed-stage recovery preserves a real accepted marker if one exists.
Otherwise it validates the original private prefix and replaces its stage header
with a version1 `MMPABT01` abort descriptor at the same encoded length. The80-byte
prefix binds occurrence, original/declaration generations, chunk count, encoded
size and original-header SHA256; remaining padding must be zero. Tail chunks are
then deleted in bounded exact-preimage transactions. Missing/corrupt rows, changed
history and unknown descriptor versions fail closed. Interrupted cleanup resumes
that same decision; it cannot switch back to acceptance. Only complete cleanup or
exact untouched absence qualifies a refusal for paired actor adoption. The final
bounded metadata tombstone stays charged, and later compaction must preserve
allocator/terminal history. Reusing the existing header cell supports cleanup at
the database page ceiling; total physical/quota qualification remains unfinished.

The lower storage-qualified ACK path reserves the original recipient interval,
offered cumulative prefix and endpoint clock without advancing the cursor. It
protects pending slots from same-recipient bypass and conflicting terminal work;
expiry exclusions prevent new rollback offers while unresolved. The linear worker
shares charged canonical sources, hashes their original acceptance encoding off
actor, and drops those references before returning a payload-free positive fact.
The version1 `MMPACK01` binary record uses little-endian fixed fields: original and
endpoint sequences, registration floor, offer clock, recipient length and entry
count, followed by recipient bytes and45-byte entries (sequence, binding index,
decision byte, source SHA256). Encoding is bounded below one256KiB transaction.
The private store verifies complete original source chunks/checksums/hash before
committing or reconciling exact ACK bytes. Missing, changed, future-format or
memory-only source cannot qualify success. Only positive actor adoption advances
slots/cursor; retired or replacement subscriptions cannot inherit that update.
Actual canonical/application commit must precede this request. Owned actor/control
scheduling, ACK headroom, journal compaction and restart folding remain unfinished;
this storage component does not enable durable runtime mode by itself.

Acceptance and ACK jobs share the same root-owned connection and settled store
position. Busy checks precede lower issuance. The root retains the ACK's actual
handle, original plan, uncertainty and positive fact independently of observers;
only original lower adoption releases its lane. Lost input requires the actual
joined worker, and retry uses fresh nonce plus monotonic backoff. Hashing and
storage run on the blocking worker. Stop closes new sends and permits ACK drains
for already accepted/application-committed input. A known foreign connection is
discarded off actor before reopening the trusted path under the original plan;
returning the original allocation can recover without another record or write.
This does not grant authority from a matching owner label or implement actor
registry/supervisor, ingress, full control headroom or restart folding.

An internal actor ACK adapter now retains the registered root and original reply,
polling actual joins both under busy ingress and while idle. After the raw
transport confirms its batch write, this path reserves the original cursor and
replies only after the ACK record commits and the actor adopts it. A cancelled
reply observer cannot discard the job; a busy lane issues nothing and a settled
duplicate writes no extra record. A completed socket write does not prove peer
reading/action execution or native context commit. Native delivery still needs
its separate canonical input/dedup boundary.

Graceful shutdown closes actor admission and retains the service/root through
issued ACK settlement, including storage retries and cancelled shutdown observers.
It does not protect canonical settlement against forced actor abort or runtime
destruction. Ordinary product construction still uses the in-memory path; the
internal root is not enabled there until native/raw/bridge acceptance, restore,
supervision and full storage quotas/control journals are integrated.

The internal raw storage adapter now parses authenticated send/task messages and
explicit ACKs separately from acceptance. It selects idle root capacity before
reserving a sequence, then retains only original connection/lease/reply metadata
beside the charged lower source. Actual commit and actor adoption precede the
send ACK; finalization never redispatches the original frame. Busy sends reserve
no occurrence. Objective changes after reservation cannot rewrite that source.
The original raw continuation also captures a monotonic selection instant.
Completion delivery and its next timer use the original logical clock plus
elapsed storage/retry time, with saturating arithmetic. Acceptance time and TTL
remain unchanged: an expired source cannot enter native input after a slow write,
while its actually committed acceptance remains historical truth. Live finite
and infinite-TTL sources still follow ordinary native delivery.

Live duplicate replies require the exact issuing root allocation, authenticated
sender, immutable envelope and audience. Memory-only receipts, a matching owner
label, generic qualification without that root, or restored snapshot data cannot
upgrade this authority. Reopened roots need actual source reconciliation; durable
lookup after replay eviction remains unfinished. The opaque live root scope is
never serialized as a reconnect or native-task capability.

Cancelled raw callers and retired socket leases do not abandon issued originals
or revive their sender identities. Active storage failures retry the same original.
On graceful shutdown, exact failed-stage reconciliation preserves a committed
acceptance or releases only a qualified noncommit/complete abort, then returns the
original wire result. Explicit raw ACKs use the same physical lane as fanout ACKs
and update the connection cursor only after adoption. Identity/discovery/presence/
receive still use their ordinary audited adapter. Normal product construction
continues without this internal root; native/bridge acceptance, native context/
dedup/restore, supervisor retention and complete quota/control integration remain.

Tracked native storage preparation now retains original recipient-task slots,
parent-process evidence and semantic fingerprint beside the lower reservation.
These slots cannot authorize a wait before acceptance, but they observe captured
task terminal/retirement/cohort-consumption events while publication is pending.
The original native/lower opaque tickets gate adoption and release. A paired
receipt lookup uses the actual admission allocation, not public IDs; an actual
lower publication followed by lost callback result can therefore finish native
metadata installation without publishing another send. Conversation and expiry
pruning retain that exact charged receipt until installation; other receipts in
the same conversation still retire independently. The original TTL is not renewed,
and ordinary pruning resumes after adoption. Consumed, restored and foreign
capabilities cannot recreate that receipt or dependency.

An internal actor tracked-native receipt entry selects idle capacity first and
retains the original runtime/lower/root owners through real join and metadata
adoption, including cancelled observers and graceful shutdown. Exact qualified
abort releases all reservations; generic error does not. Snapshot replacement
refuses unsettled acceptance/ACK/native metadata before pane teardown, even if lower publication has
already succeeded. This is a trusted harness receipt boundary, not permission
capture, final native action-result publication or canonical received-input proof.
Normal action/approval, untracked and bridge continuations still need integration;
normal product construction continues without durable mode enabled.

Only a never-issued reservation or an exactly qualified noncommit can be called
unaccepted. Ambiguous writes retain the same original for reconciliation. A lost
worker result requires actual join evidence before a new attempt; generic errors
or dropped observers do not prove absence. Repeated positive adoption reports the
original fact without replaying mail. These are lower-layer contracts: the current
runtime/native/socket ingresses still use in-memory acceptance until the durable
storage integration is complete; no new restart guarantee is advertised here.

Schema6 additionally preserves exact offered-recipient subsets and their
selection clock high-waters. Missing, duplicate, foreign and TTL-inconsistent
offer records reject before reconstruction. Schema5 mail restores its known
recipients with no offer evidence; a fresh receive/fanout selection is required
before new cursor advancement. One optional clock slot is preallocated per
accepted binding within the existing metadata reservation, not an unbounded ACK
history. Offers remain tied to the original registration through retirement and
opaque-ID reuse, and cannot establish application consumption or reconnect
rights.

Schema7 adds custody-only sources and exact committed-recipient subsets. Each
source appears once, in either replay or custody-only rows; indexes and pending
references are rebuilt from it. Missing/duplicate/foreign sources, forged commits
without genuine offers, commits beyond a still-exact live cursor, inconsistent
receipts and over-cap custody reject before adoption.
An active exact cursor cannot hide explicitly uncommitted live mail. Legitimate
unoffered expired prefixes retain a bounded ACK-endpoint expiry observation,
even after that endpoint leaves replay. This is neither a recipient offer nor
a commit or terminal outcome; invalid TTL/exclusion evidence rejects.
Snapshot round trips retain
fast-recipient commits even after that recipient retires, leaving slow recipients
independent. Schema5/6 can reconstruct only provable retained obligations, not
unknown evicted history. Unknown commits remain conservatively pending only
where the cursor permits selection. An advanced exact schema5/6 cursor without
the retained recipient's own offer evidence explicitly refuses restoration,
rather than inventing commits, rewinding or adopting inaccessible pending mail.
Over-cap legacy state also refuses rather than dropping source. Snapshot support
is not an acceptance-atomic disk write or transparent application/workflow restart.

Schema8 adds exact sender registration intervals (or explicit legacy unknown)
and fixed terminal slots in binding order, including released tombstones in
optional replay. Missing/wrong-sized slots, overlapping ACKs, pre-acceptance
observations, invalid TTL/retirement reasons and invalid sender intervals reject
before adoption. New fields under older schemas reject as well. For schemas5–7,
only a still-current sender interval covering the occurrence is provable;
retired/replaced legacy owners cannot gain handoff authority. Restored terminal
mail cannot become live again on clock rollback. The ten-minute horizon and
explicit product-integration boundary above apply equally after round trip.

## Payloads and MAAP bridge

Idle model-mail receive admission waits for checked bookkeeping of the exact
currently bound conversation, including terminal compaction append receipts.
Accepted envelopes remain behind their durable cursor until history is safe;
deferral does not create a turn or spend the peer-turn loop budget. Existing
checked-settlement and append-completion wakes resume delivery without resending
the envelope. Blocked required history stays observable and fail-closed without
failing healthy timer services or other recipients. Original TTL filtering,
bridge nonwake rules, human inhibition and exact-owned child-failure authority
remain unchanged; neither mail nor settlement authorizes effect replay.
Pending work retains the existing one-second maintenance timer and fair fanout
budgets. A deferred sweep checks the exact history fence without retrying the
archive read or constructing a receive context.

Text payloads use `text/plain; charset=utf-8`; JSON uses
`application/json`. Binary data is base64 text with `payload_encoding` set to
`base64`. Receivers enforce configured payload limits.

A MAAP `send_message` action is one way an agent asks Mezzanine to deliver an
MMP message. Its `text/plain` shorthand is normalized to the canonical MMP
text media type. MAAP action results report recipient identity, message ID when
assigned, delivery status, and MMP protocol errors; MAAP and MMP remain
separate wire protocols. After message-service acceptance, eligible
model-authored sends may produce one durable `${recipient}< {payload}` sender
presentation row; recipient rows remain separate `{sender}> {payload}` records
created only by recipient commit. Sender retention is operator-visible evidence
of acceptance or queueing, not delivery confirmation, acknowledgment, agreement,
or completion, and never participates in receiver receipts, cursors, recovery,
or MMP routing.
Both sender and recipient pane-log continuations start five display spaces
after the `▐ ` gutter, independent of endpoint-label width; Markdown structural
indentation is additive, and copied source excludes presentation-only padding.
Ordinary concrete-agent labels use the bounded, sanitized runtime-owned
subagent display name when available, otherwise the canonical raw agent id;
mutable pane, window, or conversation titles are never used as agent identity.
Receiver receipts preserve their resolved labels for retry and replay, while
non-agent selector expressions remain the explicitly addressed label.

## Related pages

- [Protocol conventions](common-conventions.md)
- [`maap/1` action protocol](maap.md)
- [Subagents and messaging](../../agent/subagents-and-messaging.md)
- [Normative MMP contract](../../../SPEC.md#12-local-message-passing-protocol)

## Native request-bound waits

A native sender's success means acceptance, independently of receiver recovery.
A recoverable receiver context/cursor/admission error is diagnosed and retried
by receiver maintenance; it cannot fail or repeat the already accepted send.
The original actual message ID/tracking result and consumed sender approval
remain authoritative. Sender presentation integrity errors still propagate,
with the accepted result retained before the error escapes.
Approved inline sends reserve their original completion owner before acceptance;
later publication or scheduler-start cleanup retains that original result
instead of making the consumed approval available for replay.

MAAP `send_message.expect_reply` and `wait.for_message`/`from_agent` are native
conversation controls, not new MMP envelope fields. Tracking freezes the actual
accepted recipient registrations and optional exact owned task at send time.
An authenticated reverse `send` with `correlation_id` equal to the accepted
request ID can supply response-occurrence evidence after canonical commit.
A genuine response accepted before its sender retires keeps precedence while
live in pending custody. Pending custody defers retirement and provider admission
but cannot itself satisfy the wait, create an offer or ACK, or count as progress.
An older in-flight response is fenced even before the context frontier advances.
Delivery and retirement use one trusted native TTL snapshot; expired input no
longer defers unavailability, including after an already recorded expiry and
clock rollback.
State-only successful completion is a terminal occurrence. A tracked exact loop
iteration can supply its result while the logical loop continues; later tasks
are never substituted for that captured iteration. Native terminal evidence is
retained before controller handoff, and failure references reuse native
custody rather than duplicating diagnostics.
Correlation text alone, bridge flags, ACKs, presence heartbeats and later reused
addresses never grant task ownership or prove a substantive answer.
Requested pane closure supplies only intentional unavailability, even when
internal cleanup records the child task as failed. It grants no progress and
cannot wake an unrelated plain wait.

Request receipts survive payload replay eviction independently of ordinary
delivery custody, under finite count and metadata reservations made before
acceptance. For a trusted envelope with a finite original wire TTL, first strict
expiry observation starts the existing ten-minute handoff window for unanswered
tracking. ACK/replay do not end this reservation early, and clock rollback does
not renew it. Actual pending/retained outcomes keep their full reservation and
source protection through normal handoff. The existing message-maintenance
cadence handles expiry; no-TTL or protected-only tracking adds no expiry timer.
Expiry itself neither wakes/settles a wait nor grants progress. A selected wait
whose tracking expires still admits ordinary mail; selecting the retired ID
later returns bounded correction. MAAP has no added TTL/deadline parameter.
 Retained allocations count their actual owned capacity, rather than
only the text length; cached output cannot retain uncharged formatting slack. Source references stay exact through context and scheduler handoff.
After unrelated mail resumes the owner, the selected outcome still enters its
next safe context boundary. Staging adds no scheduler slot and changes no settled
call result. An older response is rejected through the existing context fence;
the protected request reference remains until an accepted response from a real
dispatch that included it. Failed commits retain source and defer admission.
Restore drops same-runtime request authority rather than reconstructing it from
history. Plain waits retain their ordinary-mail and existing unexpected-owned-
failure rules. No retry, task restart, stall timer or cycle detector is implied.
