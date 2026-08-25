# Start your first session

## Purpose

Launch Mezzanine in a working directory, open the pane-local agent shell, and
complete a small reviewable task.

## Prerequisites

- Install `mez`.
- Complete [authentication](authentication.md) for model-backed agent work.
- Choose a repository or other working directory.

## Initialize configuration and start

Optionally create and inspect the baseline configuration, then start Mezzanine
from the project you want to work in. Starting a session also creates the
default configuration when none exists:

```sh
mez config init
cd /path/to/repository
mez new
```

`mez new` always creates a session. Bare `mez` first attaches to a session
that accepts a primary client, then creates one only when none is available.
Use `mez list` and `mez attach` to inspect and select existing sessions.
Creating or attaching a primary client requires an interactive terminal.

## Open the agent shell

Press `Ctrl+A a` in the focused pane. The prompt belongs to that pane, so you
can still navigate other panes and use normal multiplexer controls. Start with
a bounded request that favors inspection, such as:

> Read this crate, identify the most relevant failing or risky area, and
> propose the smallest safe fix. Start with local reads and focused commands.

Review requested approvals. Approval decisions and operating-system confinement
are separate protections; do not relax either without understanding the active
policy and sandbox.

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
