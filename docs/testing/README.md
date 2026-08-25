# Testing and performance guides

## Purpose

Collect specialized benchmark and release-evidence procedures that complement
the required contributor checks without presenting report-only measurements as
functional correctness gates.

## Prerequisites

Read [Development and validation](../contributing/development-and-validation.md)
and follow the repository [AGENTS.md](../../AGENTS.md) before running or changing
these workloads.

## Guides

- [Iroh compression benchmarks](iroh-compression-benchmarks.md): reproduce the
  application-frame codec measurements used for transport rollout decisions.
- [Cross-platform release load checks](release-load-checks.md): collect and
  interpret report-only responsiveness evidence on Linux and macOS.

## Related pages

- [Contributing](../contributing/README.md)
- [Iroh production operations and rollout](../operations/iroh-production-operations-and-rollout.md)
- [Operations and troubleshooting](../operations/README.md)

## Next step

Choose the guide for the affected subsystem, retain generated reports under
`target/`, and report the exact environment and command with any measurements.
