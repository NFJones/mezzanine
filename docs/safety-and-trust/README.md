# Safety, trust, and security

## Purpose

Explain approval decisions, OS confinement, project trust, instructions, and
security-relevant diagnostics.

## Prerequisites

Read the [agent overview](../agent/overview.md). [SPEC.md](../../SPEC.md) states
the normative requirements, not proof that every requirement is implemented.
The chapters below identify current behavior and important gaps.

## Boundary checklist

- Approval is permission to act, not confinement or proof of harmlessness.
- Inspect effective sandbox enforcement, not only the configured backend.
- Review project trust and connector exposure independently of shell isolation.
- Native patches remain shell-backed; planned process-free filesystem actions
  are not integrated.
- Required audit logging currently has an asynchronous durability gap. Review
  [audit limits](audit-and-diagnostics.md) before relying on it for compliance.

## Chapters

Review the boundary relevant to your task:

- [Approvals and review](approvals-and-review.md)
- [Sandboxing](sandboxing.md)
- [Project trust and instructions](project-trust-and-instructions.md)
- [Remote pairing and recovery](remote-pairing-and-recovery.md)
- [Audit and diagnostics](audit-and-diagnostics.md)

## Related pages

- [Configuration](../configuration/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Agent and integrations](../agent/README.md)

## Next step

Start with [Approvals and review](approvals-and-review.md) before changing a
policy or approving an agent action.
