# Workflows

## Purpose

Apply Mezzanine's session and agent features to routine, reviewable work.

## Prerequisites

Know [Sessions and panes](sessions-and-panes.md) and the [Agent shell](agent-shell.md).

## Investigate and change a repository

Start with a narrow request that names the goal and desired validation. For
example:

> Find the owner of this failing test, make the smallest safe correction, run
> the focused check, and summarize the result.

Review requested actions before approval. Keep independent investigations in
separate panes when useful; each pane owns its own agent conversation and
working context. Ask for a concise plan first when the change is broad or a
decision needs review.

## Use reusable prompts and coordination

Begin a prompt with `$<skill-name>` to invoke an available skill or
`#<macro-name>` for an ordered macro. Use `@<mcp-server-name>` only when the
task needs a configured MCP integration; injected tool details apply to that
turn rather than becoming permanent context.

Use subagents for bounded, separable work and keep the parent responsible for
integration. See the agent guide for routing, messaging, continuity, and
provider behavior.

## Forward X11 applications from a remote session

X11 forwarding is an explicit authenticated-Iroh-primary workflow. The host
must enable `[transport.iroh.x11]`, and the attaching machine must already have
a supported local `DISPLAY`, a matching `MIT-MAGIC-COOKIE-1` record, and
`xauth`. Pair the client normally, then request untrusted forwarding:

```console
mez --iroh-profile home-mez attach --x11
```

Programs launched in the remote panes inherit a stable session-local
`DISPLAY` and `XAUTHORITY`. Mezzanine forwards their X11 sockets to the X
server selected on the attaching machine. `--x11` requests an X SECURITY
untrusted credential and fails closed if the local X server or `xauth` cannot
create one. It never falls back to trusted forwarding.

Use `--x11-trusted` only when the application requires full X11 authority and
the host explicitly sets `transport.iroh.x11.allow_trusted = true`. If another
primary already owns the session route, reconnect with `--x11-takeover` only
when intentionally replacing that route:

```console
mez --iroh-profile home-mez attach --x11-trusted --x11-takeover
```

X11 forwarding is unavailable to observers and Unix-socket attachments. A
detach, takeover, trust or lease revocation, transport loss, or session stop
invalidates the old route and closes its X streams. Reattach rotates the route
cookie, generation, and token while preserving the remote pane environment;
applications must open new X connections after reattach.

The supported scope is conventional X11 and Xwayland socket traffic. Native
Wayland, audio, D-Bus, desktop portals, device forwarding, MIT-SHM across the
network, and reliable direct GLX/DRI acceleration are not forwarded.

## Recover and continue

Use `/status` to inspect the current pane's model, policy, context, and token
state. Use `/stop` to interrupt work, `/new` to start a fresh conversation, and
`/resume` to return to a saved one. The first non-slash prompt after `/stop`
continues from the interrupted turn's retained model context and can either
resume its direction or redirect it; cancelled actions and processes are not
restarted. Detach and reattach sessions when the client must leave; use
operations guidance for service, diagnostic, or recovery issues.

## Related pages

- [Agent and integrations](../agent/README.md)
- [Operations and troubleshooting](../operations/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)

## Next step

Read [Agent and integrations](../agent/README.md) for commands, skills,
subagents, providers, and MCP.
