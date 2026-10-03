# Operations and troubleshooting

## Purpose

Guide persistent-host operation, lifecycle management, recovery, diagnostics,
cache inspection, power inhibition, optional remote transport rollout, and
symptom-based troubleshooting.

## Prerequisites

Know the basic session workflow in [Using Mezzanine](../using-mezzanine/README.md).

## Before recovery or restart

Preserve private diagnostics and identify the affected host, session, and pane.
Detach leaves background work running; stopping or restarting the owning host
does not. A saved layout or conversation is not a live process checkpoint.
Treat uncertain command effects as possibly applied and inspect them before
retrying. Keep a tested local Unix administration path before enabling remote
access, and review diagnostic exports before sharing them.

## Chapters

This section owns the following operational guidance:

- [Persistent multi-session host](persistent-host.md)
- [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md)
- [Cache status and diagnostics](cache-status-and-diagnostics.md)
- [Power inhibition](power-inhibition.md)
- [Troubleshooting](troubleshooting.md)
- [Iroh production operations and rollout](iroh-production-operations-and-rollout.md)

## Related pages

- [Configuration](../configuration/README.md)
- [Remote pairing and recovery](../safety-and-trust/remote-pairing-and-recovery.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

Start with [Persistent multi-session host](persistent-host.md) for a
service-manager deployment, or [Lifecycle, detach, and
recovery](lifecycle-detach-and-recovery.md) when operating one session.
