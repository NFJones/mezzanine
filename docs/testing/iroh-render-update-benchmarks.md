# Iroh render-update benchmarks

## Purpose

Collect reproducible, content-safe evidence for Iroh render-update latency,
payload selection, compression cadence, and backpressure. Keep the local
release report separate from controlled network measurements: the report
measures update selection and codec work and models serialized request RTTs,
while the WAN matrix measures complete direct or relayed sessions.

## Local release report

Run from the repository root on an otherwise reasonably idle machine:

```text
just iroh-render-bench
```

The single-threaded release test writes `target/iroh-render-bench.json`. Set
`MEZ_IROH_RENDER_BENCH_REPORT` to an absolute or repository-relative output
path. The report contains no terminal text, endpoint identity, address, relay
URL, credential, or topology label.

The report records:

- one-row delta, broad-row snapshot, and invalidating-snapshot fixtures;
- selected update kind, changed-row count, selected decoded and wire bytes,
  and the corresponding full-snapshot candidate bytes;
- identity, zstd level 3, and LZ4 selection/encode/decode latency percentiles;
- one independently decodable envelope and one immediate flush per update;
- a serialized-request model for v2 and v3 at 0, 25, 75, and 150 ms RTT.

The RTT section is a request-count model, not a network measurement. It makes
the avoidable v2 `terminal/view` round trip explicit and deliberately excludes
server rendering, codec work, scheduling, relay behavior, and client apply
time. Do not report its values as measured end-to-end latency.

## Live content-safe counters

During a controlled session, `show-iroh-status` reports the invoking
connection's interval values for:

- `terminal/step` and `terminal/view` requests through actor metrics;
- render composition and encoding histograms through `show-metrics`;
- snapshot and delta counts, changed rows, selected wire/decoded bytes, and
  full-snapshot candidate bytes;
- compressed versus identity envelopes and total compression effectiveness;
- coalesced triggers, suppressed identical updates, snapshot fallbacks,
  maximum ready depth, and total/maximum render write-and-flush wait; and
- RTT, jitter, loss, congestion, and path class without endpoint or address
  disclosure.

Counters are connection-local or process-local aggregates, not payload traces.
Capture them before and after each workload and subtract the baseline. Use a
fresh connection when comparing codecs or protocol versions.

## Controlled WAN matrix

Run the same build and workload on each row below. Record at least 30 samples
per workload after five warm-up samples. Keep direct and relay runs separate.

| Run | RTT | Jitter/loss |
| --- | ---: | --- |
| baseline | 0 ms | none |
| latency-25 | 25 ms | none |
| latency-75 | 75 ms | none |
| latency-150 | 150 ms | none |
| jitter | 75 ms | 5 ms normal jitter |
| loss | 75 ms | 1% random loss |

Use a dedicated test host, namespace, VM, or interface. Never apply traffic
control to an administrator's production interface. On Linux, apply half of
the target RTT to egress at each endpoint. For example, with client interface
`$CLIENT_DEV` and server interface `$SERVER_DEV`:

```text
sudo tc qdisc replace dev "$CLIENT_DEV" root netem delay 37.5ms
sudo tc qdisc replace dev "$SERVER_DEV" root netem delay 37.5ms
```

For the separate jitter run:

```text
sudo tc qdisc replace dev "$CLIENT_DEV" root netem delay 37.5ms 2.5ms distribution normal
sudo tc qdisc replace dev "$SERVER_DEV" root netem delay 37.5ms 2.5ms distribution normal
```

For the separate loss run:

```text
sudo tc qdisc replace dev "$CLIENT_DEV" root netem delay 37.5ms loss 0.5%
sudo tc qdisc replace dev "$SERVER_DEV" root netem delay 37.5ms loss 0.5%
```

Remove the rules after every run, including failed runs:

```text
sudo tc qdisc del dev "$CLIENT_DEV" root
sudo tc qdisc del dev "$SERVER_DEV" root
```

Confirm the measured ping RTT before collecting application samples. If only
one egress can be shaped, document that fact and use the full one-way delay
needed to obtain the target measured RTT; do not compare that run directly
with a symmetric two-endpoint run.

For direct runs, use pinned direct addresses with relay and public lookup
disabled. For relayed runs, use the approved custom relay with direct
connections disabled and verify the selected path is `relay` before sampling.
Never substitute the public relay for an approved controlled relay or silently
fall back from relay to direct.

## Workloads and capture

Exercise these bounded workloads for primary and differently sized observer
clients:

1. shell character echo and command submission;
2. one-row cursor-rewrite progress;
3. bursty multi-line output;
4. alternate-screen entry, activity, resize, and exit;
5. incremental agent/provider presentation;
6. idle status and animation refresh;
7. reconnect after a completed update; and
8. a slow-consumer burst that forces write backpressure.

For each sample record input-send to first relevant visible update, inter-frame
gap p50/p95/p99, step/view counts, snapshot/delta/suppression/coalescing counts,
selected and candidate bytes, render composition/encoding, write wait, and
client decode/apply/render duration. Record the codec, protocol version, path
class, measured RTT, workload, build commit, operating system, CPU, and relay
deployment identifier. Keep terminal contents and credentials out of reports.

Client-side input-to-visible and decode/apply/render timings are end-to-end
measurements collected by the benchmark operator; the server aggregates do not
claim to observe physical terminal presentation.

## Release interpretation

- Negotiated primary and observer v3 workloads must issue zero steady-state
  `terminal/view` requests after the initial snapshot.
- At 75 ms RTT, an immediately rendered v3 action should avoid approximately
  one v2 view-fetch RTT and be materially faster without regressing the local
  direct case.
- The representative one-row delta must use at most 50% of the decoded bytes
  of its full-snapshot candidate.
- A blocked-link burst must keep bounded latest-state work and avoid a stale
  replay tail.
- Identity, LZ4, and zstd must retain one envelope and one immediate flush per
  update. Compare cadence distributions; do not infer batching from codec
  throughput alone.
- Treat one noisy run as report-only evidence. Require repeated samples and
  document infrastructure incidents before setting hard performance gates.

## Related pages

- [Iroh compression benchmarks](iroh-compression-benchmarks.md)
- [Iroh production operations and rollout](../operations/iroh-production-operations-and-rollout.md)
- [Cross-platform release load checks](release-load-checks.md)

## Next step

Retain generated reports under `target/`, retain separately captured WAN
results with their environment metadata, and compare like-for-like builds
before changing rollout policy or compression defaults.
