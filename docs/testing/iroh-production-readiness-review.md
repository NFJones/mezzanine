# Iroh production-readiness review packet

## Purpose

Use this packet to review issue
`8c2e5980-b58e-43b3-9106-507a97e02211` without confusing repository-local
correctness with production readiness. It records the candidate build,
infrastructure decision, native-platform and network evidence, operational
drills, exceptions, and approval decision in one auditable place.

The repository implementation reference is `ab5cc64d` (`Harden Iroh production
operations`). Reviewers must evaluate one exact candidate revision and attach
all generated evidence from that same revision. Do not approve a mixture of
results from different builds.

This file is a review template, not an approval. Every field marked **Required**
must be completed, or represented by an approved exception, before the issue is
resolved. Do not put credentials, private keys, invitations, endpoint IDs,
peer addresses, terminal contents, or secret-bearing configuration in this
record or its attachments.

## Review identity

| Field | Review value |
| --- | --- |
| Issue | `8c2e5980-b58e-43b3-9106-507a97e02211` |
| Candidate commit | **Required:** exact full Git SHA |
| Candidate version/package | **Required:** version and package identifiers |
| Review opened | **Required:** UTC date and time |
| Evidence cutoff | **Required:** UTC date and time |
| Release owner | **Required:** name/team |
| Security reviewer | **Required:** name/team |
| Networking/relay reviewer | **Required:** name/team |
| Operations/SRE reviewer | **Required:** name/team |
| Linux test owner | **Required:** name/team |
| macOS test owner | **Required:** name/team |
| Final decision owner | **Required:** name/team |

Record generated reports in an access-controlled evidence store. For each
attachment, retain the candidate commit, command or procedure, platform,
timestamp, operator, and SHA-256 digest.

## Decision requested

Select exactly one outcome after reviewing every gate:

- [ ] **Approve supported opt-in production use.** Every gate passed or has an
  approved, owner-assigned exception.
- [ ] **Approve controlled opt-in preview only.** Production support remains
  blocked; scope, operators, devices, expiry, and rollback authority are
  recorded below.
- [ ] **Reject or defer.** Blocking gates and the next review date are recorded
  below.

Iroh must remain disabled by default. Public n0 relay and lookup services are
development or controlled-evaluation dependencies, not an implicit production
choice.

## Production infrastructure approval

Choose one explicit policy: direct only, relay required, or controlled direct
plus relay. Copy the non-secret approved configuration into the review record
and validate it with `mez config validate`. The policy examples and constraints
are in [Iroh production operations and
rollout](../operations/iroh-production-operations-and-rollout.md).

| Required decision | Owner | Approved value or attachment | Reviewer | Status |
| --- | --- | --- | --- | --- |
| Network policy and permitted fallback behavior | **Required** | **Required** | **Required** | Pending |
| Relay service, domains, and regions | **Required** | **Required or N/A for direct only** | **Required** | Pending |
| Endpoint-address lookup service and domain | **Required** | **Required or disabled** | **Required** | Pending |
| Incident contact and escalation path | **Required** | **Required** | **Required** | Pending |
| Availability, detection, and recovery targets | **Required** | **Required** | **Required** | Pending |
| Capacity, bandwidth, churn, and rate limits | **Required** | **Required** | **Required** | Pending |
| Authentication, proxy, CA, and trust-store policy | **Required** | **Required** | **Required** | Pending |
| Metadata fields, residency, retention, and deletion | **Required** | **Required** | **Required** | Pending |
| Service upgrade, compatibility, and rollback owner | **Required** | **Required** | **Required** | Pending |

Any missing or unapproved row blocks supported production use.

## Evidence manifest

Add one row for every report, log bundle, test record, package, configuration,
or approval used by the review. Links must be immutable or content-addressed.

| ID | Gate | Candidate commit | Platform/infrastructure | Procedure or command | Artifact link and SHA-256 | Operator/date | Result |
| --- | --- | --- | --- | --- | --- | --- | --- |
| E-001 | Repository validation | **Required** | **Required** | **Required** | **Required** | **Required** | Pending |
| E-002 | Linux native run | **Required** | Linux | See native-platform matrix | **Required** | **Required** | Pending |
| E-003 | macOS native run | **Required** | macOS | See native-platform matrix | **Required** | **Required** | Pending |
| E-004 | Direct network matrix | **Required** | Controlled direct | See WAN matrix | **Required** | **Required** | Pending |
| E-005 | Relay network matrix | **Required** | Approved custom relay | See WAN matrix | **Required** | **Required** | Pending |
| E-006 | Abuse/resource measurements | **Required** | **Required** | See abuse matrix | **Required** | **Required** | Pending |
| E-007 | Package/upgrade/rollback drill | **Required** | Linux and macOS packages | See operational drills | **Required** | **Required** | Pending |
| E-008 | Telemetry/privacy review | **Required** | Production policy | See telemetry review | **Required** | **Required** | Pending |

Extend the table rather than combining unrelated evidence into one row.

## Release-readiness gates

Repository tests are supporting evidence only. A gate is `Pass` when its
required release evidence is attached and accepted. Use `Exception` only when
the exception register contains an approved owner and due date.

| Gate | Required review evidence | Evidence IDs | Reviewer | Status |
| --- | --- | --- | --- | --- |
| Disabled default and Unix recovery | Packaged disable/restart drill proves network activity stops, Unix administration remains available, explicit remote targets fail visibly, and no session migration is required. | **Required** | **Required** | Pending |
| Policy validation | Approved production configuration passes validation and matches the selected route, relay, lookup, proxy, and CA policy. | **Required** | **Required** | Pending |
| Direct path | Native Linux and macOS attach, detach, control, events, timeout, malformed traffic, abrupt loss, reconnect, and stream-limit runs. | **Required** | **Required** | Pending |
| Relay-required and direct-plus-relay | Approved custom-relay path, outage, migration, latency, throughput, and reconnect runs; selected relay path verified before sampling. | **Required** | **Required** | Pending |
| Lookup | Ownership and retention approval plus DNS loss, route refresh, recovery, and pinned endpoint-identity verification. | **Required** | **Required** | Pending |
| Network diversity | LAN, representative NAT, IPv4, IPv6, proxy/CA, latency, jitter, loss, and reordering results. | **Required** | **Required** | Pending |
| Abuse and bounds | Descriptor, memory, CPU, connection/stream flood, queue pressure, malformed/oversized work, slow-consumer, and concurrent-session measurements show bounded behavior and no authority leakage. | **Required** | **Required** | Pending |
| Privacy and telemetry | Production schema and retention review confirms aggregate diagnostics are sufficient and excludes secrets and unnecessary peer metadata. | **Required** | **Required** | Pending |
| Performance and package impact | Package and binary size, cold startup, idle/active CPU and memory, direct/relay latency and throughput, reconnect, observer fan-out, and concurrent-session baselines. | **Required** | **Required** | Pending |
| Compatibility | Packaged new/new, new/old, and old/new client/server runs plus upgrade and rollback with supported protocol fallback. | **Required** | **Required** | Pending |
| Client clipboard | Native Linux/macOS desktop and headless runs prove exact-client routing, isolation, malformed/timeout handling, local command ownership, and legacy fallback. | **Required** | **Required** | Pending |
| Operations and recovery | Listener failure, relay/lookup outage, proxy/CA failure, key loss/rotation, revocation, trust-store recovery, disable, restart, and rollback runbooks exercised. | **Required** | **Required** | Pending |

## Native-platform acceptance matrix

Run the same candidate packages on native Linux and macOS. Record package
source, operating-system version, architecture, CPU, memory, shell, terminal,
network interfaces, proxy scope, CA mode, and whether the run was desktop or
headless.

| Scenario | Linux evidence | macOS evidence | Acceptance condition |
| --- | --- | --- | --- |
| Install and cold start | **Required** | **Required** | Package installs cleanly; startup baseline recorded. |
| Unix-only default | **Required** | **Required** | Iroh disabled; Unix control works; no remote listener activity. |
| Direct attach/detach/reconnect | **Required** | **Required** | Correct session and authority; bounded reconnect. |
| Forced relay attach/detach/reconnect | **Required** | **Required** | Selected path is relay; no silent direct or Unix fallback. |
| Observer fan-out and resize | **Required** | **Required** | Correct client-local geometry and bounded update work. |
| Clipboard desktop/headless | **Required** | **Required** | Only the negotiated primary receives client-local effect. |
| Revocation and old-profile rejection | **Required** | **Required** | Revoked or superseded credentials cannot initialize. |
| Upgrade and rollback | **Required** | **Required** | Supported compatibility works and Unix recovery survives. |
| Disable and restart | **Required** | **Required** | Network activity stops and Unix administration remains usable. |

## Controlled direct and relay network matrix

Follow [Iroh render-update
benchmarks](iroh-render-update-benchmarks.md). Collect at least 30 samples per
workload after five warm-up samples. Keep direct and approved custom-relay runs
separate, confirm measured ping RTT, and verify the selected path before each
sample set.

| Path | Condition | Target | Evidence ID | Result |
| --- | --- | ---: | --- | --- |
| Direct | Baseline | 0 ms RTT | **Required** | Pending |
| Direct | Latency | 25 ms RTT | **Required** | Pending |
| Direct | Latency | 75 ms RTT | **Required** | Pending |
| Direct | Latency | 150 ms RTT | **Required** | Pending |
| Direct | Jitter | 75 ms RTT, 5 ms normal jitter | **Required** | Pending |
| Direct | Loss | 75 ms RTT, 1% random loss | **Required** | Pending |
| Relay | Baseline | 0 ms added RTT | **Required** | Pending |
| Relay | Latency | 25 ms RTT | **Required** | Pending |
| Relay | Latency | 75 ms RTT | **Required** | Pending |
| Relay | Latency | 150 ms RTT | **Required** | Pending |
| Relay | Jitter | 75 ms RTT, 5 ms normal jitter | **Required** | Pending |
| Relay | Loss | 75 ms RTT, 1% random loss | **Required** | Pending |

For primary and differently sized observers, exercise shell echo/submission,
cursor-rewrite progress, burst output, alternate screen and resize,
incremental provider output, idle animation, reconnect, and a slow-consumer
burst. Capture input-to-visible timing, inter-frame p50/p95/p99, steps/views,
snapshot/delta/suppression/coalescing counts, selected/candidate bytes, render
and encoding time, write wait, and client decode/apply/render time.

## Abuse and resource matrix

Record the configured bound, offered load, observed peak, steady-state value,
recovery behavior, and whether rejected work obtained any application
authority.

| Scenario | Required measurements | Evidence ID | Result |
| --- | --- | --- | --- |
| Connection churn and flood | Accepted/rejected/setup counts, descriptors, CPU, RSS, latency, recovery | **Required** | Pending |
| Stream flood | Stream limit behavior, CPU, RSS, task/descriptor recovery | **Required** | Pending |
| Oversized/malformed frames | Reject class, allocation peak, connection isolation, authority state | **Required** | Pending |
| Queue pressure | Queue depth, coalescing/suppression, memory bound, recovery | **Required** | Pending |
| Slow consumer | Write wait, bounded latest-state work, disconnect/recovery behavior | **Required** | Pending |
| Concurrent sessions/observers | CPU, RSS, descriptors, throughput, observer fan-out | **Required** | Pending |
| Shutdown under load | Drain duration, abort count, leaked tasks/descriptors | **Required** | Pending |

## Reproducible local reports

Run these from the candidate checkout and attach their generated JSON with
digests. They complement, but do not replace, controlled network and packaged
platform evidence.

```text
just iroh-compression-bench
just iroh-render-bench
just release-load-check
just release-load-sweep
```

The default reports are under `target/`. Also attach package and stripped
binary sizes, cold-start timings, and repeated idle/active resource samples.
Do not define a hard performance threshold from one noisy run. Record approved
thresholds or baseline-relative tolerances before evaluating the candidate.

## Telemetry and privacy review

| Review question | Reviewer finding | Evidence ID | Status |
| --- | --- | --- | --- |
| Are startup, listener state, relay/lookup reachability, path class, setup latency, rejects, resets, reconnects, revocations, queue pressure, and disconnect classes diagnosable? | **Required** | **Required** | Pending |
| Are credentials, private keys, invitations, payloads, terminal contents, endpoint IDs, peer addresses, relay URLs, and trust records excluded where documented? | **Required** | **Required** | Pending |
| Are every retained field, purpose, access policy, region, retention period, and deletion procedure approved? | **Required** | **Required** | Pending |
| Can operators correlate incidents without weakening client isolation or collecting payload-derived samples? | **Required** | **Required** | Pending |
| Do clean shutdowns record zero aborts, or is every nonzero result investigated? | **Required** | **Required** | Pending |

## Operational drills

For every drill, record start/end time, operator, candidate package, initial
state, actions, expected result, actual result, diagnostics, recovery time, and
artifact digest.

| Drill | Required outcome | Evidence ID | Status |
| --- | --- | --- | --- |
| Canary enable and verification | Local Unix administration retained; paired remote succeeds; listener/path diagnostics observed. | **Required** | Pending |
| Relay or lookup outage | Policy fails closed as configured; no implicit transport fallback; Unix recovery remains available. | **Required** | Pending |
| Proxy or CA failure | Failure is visible; certificate verification is not weakened; Unix recovery remains available. | **Required** | Pending |
| Listener-task failure | Service manager recovery is verified and paired Unix listener impact is understood. | **Required** | Pending |
| Connection flood/malformed traffic | Bounds hold, rejected work gains no authority, and shutdown remains bounded. | **Required** | Pending |
| Client revocation | Existing authorization is removed and future initialization fails. | **Required** | Pending |
| Server key loss/rotation | Old profiles fail, trust is replaced as a unit, and clients are re-paired through Unix control. | **Required** | Pending |
| Trust-store corruption/recovery | Corrupt state is preserved offline, clean state restored, and no hand-edited authority is accepted. | **Required** | Pending |
| Disable and restart | Iroh network activity stops; status is disabled with no endpoint ID; Unix attach and administration work. | **Required** | Pending |
| Packaged upgrade and rollback | Compatibility window is honored and rollback requires no session-data migration. | **Required** | Pending |

## Stop and rollback criteria

Stop or roll back immediately for any authority leak, secret-bearing telemetry,
remote-to-Unix fallback, Unix recovery failure, cross-session disclosure,
revocation bypass, unbounded resource growth, stale-frame replay, data
corruption, or clean-shutdown abort.

Pause a canary for investigation when setup failures exceed 5 percent over 15
minutes, setup latency reaches 80 percent of the configured timeout, or
connection capacity is sustained for 5 minutes. A stricter approved threshold
may replace these values. Any relaxation requires a recorded exception.

Rollback authority: **Required**

Rollback communication channel: **Required**

Rollback completion objective: **Required**

## Exception register

An exception must be explicit, time-bounded, narrower than the blocked gate,
and accepted by the release, security, and relevant operational owner. An
unowned or overdue exception is a failed gate.

| Exception ID | Blocked gate and evidence gap | Scope and compensating control | Risk | Owner | Due date | Required approvers | Approval links | State |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| X-001 | **Fill only if needed** | **Required** | **Required** | **Required** | **Required** | **Required** | **Required** | Draft |

Delete the placeholder row when no exceptions are requested.

## Final review record

| Reviewer role | Name | Decision | Date | Signature/approval link | Conditions |
| --- | --- | --- | --- | --- | --- |
| Release owner | **Required** | Approve / Reject | **Required** | **Required** | **Required or none** |
| Security | **Required** | Approve / Reject | **Required** | **Required** | **Required or none** |
| Networking/relay | **Required** | Approve / Reject | **Required** | **Required** | **Required or none** |
| Operations/SRE | **Required** | Approve / Reject | **Required** | **Required** | **Required or none** |
| Linux platform | **Required** | Approve / Reject | **Required** | **Required** | **Required or none** |
| macOS platform | **Required** | Approve / Reject | **Required** | **Required** | **Required or none** |

Final disposition: **Required: supported opt-in / controlled preview only /
deferred**

Decision rationale and unresolved risks: **Required**

Approved rollout scope, operators, devices, regions, and expiry: **Required**

Next review date: **Required unless fully approved**

Resolve the issue only when every release-readiness gate is `Pass` or every
remaining gap has an approved exception with an owner and due date. Attach the
completed packet or its immutable location to the issue before resolution.

## Related pages

- [Iroh production operations and rollout](../operations/iroh-production-operations-and-rollout.md)
- [Iroh pushed-render rollout evidence](iroh-pushed-render-rollout-evidence.md)
- [Iroh render-update benchmarks](iroh-render-update-benchmarks.md)
- [Iroh compression benchmarks](iroh-compression-benchmarks.md)
- [Cross-platform release load checks](release-load-checks.md)
- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
