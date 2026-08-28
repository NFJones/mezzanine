# Iroh pushed-render rollout evidence

## Scope

This record separates repository-local acceptance evidence from production and
representative-network evidence. Local tests and report-only models do not prove
relay behavior, real WAN latency, native macOS behavior, packaged upgrade and
rollback, or production telemetry safety.

## Integrated build

- Commit: `9b560fc4` (`Instrument Iroh render updates`)
- Platform: Linux 6.18.33.2 under WSL2, x86_64, glibc 2.39
- Rust: `rustc 1.97.1 (8bab26f4f 2026-07-14)`
- Reports: `target/iroh-compression-bench.json` and
  `target/iroh-render-bench.json`

Generated reports stay under `target/` and are not committed because timing is
host-dependent. Reproduce them with `just iroh-compression-bench` and
`just iroh-render-bench`.

## Repository-local gates

| Gate | Local evidence | Result |
| --- | --- | --- |
| Primary v3 render ownership | Focused attach-loop coverage renders the initial pushed frame without issuing `terminal/view`; runtime Iroh tests exercise snapshot and delta transport. | Pass |
| Observer v3 render ownership | Two-sided metadata/capability negotiation, exact-client geometry, observer-local resize, zero-fetch attach-loop coverage, and a direct-listener snapshot/resize test. | Pass |
| Legacy compatibility | Primary negotiation remains `3 → 2 → 1`; observer negotiation remains `3 → 1`; downgrade is limited to structured unsupported-version results; missing observer push opt-in or capability retains notification-plus-fetch behavior. | Pass |
| Inline input response | New servers may return a changed view inline; a null old-server response causes exactly one legacy view fetch. | Pass |
| Delta size | The report's one-row fixture selects a delta for identity, zstd, and LZ4. Its decoded size is 24.63% of the full-snapshot candidate, below the 50% gate. | Pass |
| Compression cadence | Every measured fixture records one independently decodable envelope and one immediate flush per selected update for identity, zstd, and LZ4. | Pass for mechanism; WAN cadence distribution pending |
| Backpressure | The writer keeps one encoded update in flight, coalesces bounded trigger metadata, renders latest state after the write, and forces a snapshot at the classification safety bound. | Pass for deterministic repository coverage; controlled stalled-link measurement pending |
| Request-RTT model | At 0/25/75/150 ms RTT, v2 models one step plus one view request and v3 models one step with zero steady-state view requests. The avoidable v2 view-fetch cost is 0/25/75/150 ms respectively. | Pass as a model only |
| Privacy | Status and benchmark reports use counts, durations, sizes, codec, and path class without terminal contents, endpoint identity, addresses, relay URLs, or credentials. | Pass for repository output |

The RTT values above are a serialized-request model, not measured network
latency. They exclude server rendering, codec work, scheduling, relay behavior,
and client apply/render time.

## Validation

The integrated implementation passed formatting, workspace checking, clippy,
focused control, attach, actor rendering, runtime Iroh, mux session, metric,
status, direct-listener, compression benchmark, render benchmark, report-schema,
and diff checks during the prerequisite issues.

The bounded all-target suite remained red in unrelated shell-bootstrap and
agent-layout tests whose generated logical records exceed the existing
1,024-byte bound. No pushed-render, observer-v3, metric, status, or benchmark
test failed in those runs.

## Outstanding release evidence

Do not mark the pushed-render rollout complete or production-supported until
the following evidence is attached to the rollout tracker or covered by an
owner-approved exception:

1. Controlled direct and approved custom-relay measurements at 0, 25, 75, and
   150 ms measured RTT, with at least 30 samples per workload after warm-up.
2. Separate 75 ms jitter and loss runs using the documented shaping procedure.
3. End-to-end input-to-visible and client decode/apply/render timings for shell
   echo/submission, cursor-rewrite progress, burst output, alternate screen,
   incremental provider output, animations, reconnect, and slow consumers.
4. Statistical identity-versus-zstd cadence comparison under the same direct
   and relay conditions.
5. Native Linux and macOS runs, including differently sized observers.
6. Packaged new/new, new/old, and old/new client-server upgrade and rollback
   drills with v1/v2 fallback retained.
7. Approved relay and lookup ownership, telemetry schema/retention review,
   resource and connection-flood measurements, and staged rollout sign-off.

## Rollback boundary

Keep the Unix recovery path working throughout rollout. Stop or roll back for
authority leakage, secret-bearing telemetry, cross-session disclosure,
revocation bypass, unbounded resource growth, stale-frame replay, data
corruption, or any implicit remote-to-Unix fallback. Disable Iroh through the
documented configuration and restart the supervised host; do not weaken
authentication, authorization, malformed-data handling, or downgrade rules.

## Related pages

- [Iroh render-update benchmarks](iroh-render-update-benchmarks.md)
- [Iroh compression benchmarks](iroh-compression-benchmarks.md)
- [Iroh production operations and rollout](../operations/iroh-production-operations-and-rollout.md)

