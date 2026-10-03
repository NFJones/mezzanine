# Start your first session

## Purpose

Launch Mezzanine in a working directory, open the pane-local agent shell, and
complete a small reviewable task.

## Prerequisites

- Install `mez`.
- Complete [authentication](authentication.md) for model-backed agent work,
  or configure a compatible local backend that needs no credentials.
- Choose a repository or other working directory.

## Initialize configuration and start

Start Mezzanine from the project you want to work in. Starting a session also
creates the default configuration when none exists:

```sh
cd /path/to/repository
mez new
```

To create and inspect the baseline configuration before starting a session,
run `mez config init` first. It creates the default user configuration only
when it is missing.

`mez new` always creates a session. Bare `mez` first attaches to a session
that accepts a primary client, then creates one only when none is available.
Use `mez list` and `mez attach` to inspect and select existing sessions.
Creating or attaching a primary client requires an interactive terminal.

## Open the agent shell

With the default bindings, press `Ctrl+A`, release it, then press `a` in the
focused pane. The prompt belongs to that pane, so you
can still navigate other panes and use normal multiplexer controls. Start with
a bounded request that favors inspection, such as:

> Read this crate, identify the most relevant failing or risky area, and
> propose the smallest safe fix. Start with local reads and focused commands.

Before submitting the request, inspect `/sandbox status` and `/approval`.
First-run defaults may allow actions without asking, and `policy-only` provides
no operating-system confinement. Review any requested approvals. Approval
decisions and confinement are separate protections; do not relax either without
understanding the active policy and sandbox.

For a first read-only investigation, enter `/plan on` before the request. This
enables plan-only mode for subsequent turns and removes the pane's write sandbox
scopes. Enter `/plan off` when you are ready to allow changes; use `/plan status`
to check the mode. Plan mode instructs the agent not to make changes; it is not
an OS-enforced read-only boundary under `policy-only` or sandbox bypass.
Use `/status` to inspect the active model and policy, and `/help` for the
available agent controls.

## Leave and resume work

Press `Ctrl+A d` to detach the primary client while leaving the session running
under the usual configuration. Use `mez list` to discover resumable sessions,
`mez attach <session-id>` to return, and `mez kill <session-id> --force` to
terminate one explicitly.

## Related pages

- [Sessions and panes](../using-mezzanine/sessions-and-panes.md)
- [Agent shell](../using-mezzanine/agent-shell.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [CLI reference](../reference-manual/cli.md)

## Next step

Continue to [Using Mezzanine](../using-mezzanine/README.md) for routine pane,
terminal, and agent workflows.
