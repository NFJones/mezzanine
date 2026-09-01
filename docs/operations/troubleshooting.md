# Troubleshooting

## Purpose

Diagnose common Mezzanine symptoms with bounded checks, then follow the
canonical owner for configuration, safety, provider, or terminal remediation.

## Prerequisites

Identify the affected pane or session and preserve any useful diagnostic output
before restarting or changing policy.

## Session cannot be found or reattached

Run `mez list`, then select the intended session with `mez attach <session-id>`
or select its control socket with `-S` or `-L`. An existing primary does not by
itself block another one: a session accepts up to 16 attached primaries. If the
session reports that it does not accept another primary, use another session,
request observer access, or detach a primary after confirming the session has
reached its capacity. If a background daemon started by `mez new` failed,
inspect its private `<control-socket>.diagnostics.log` before creating a
replacement session. A foreground `mez serve` reports its diagnostics to the
terminal that started it.

## Agent cannot run a shell command

Return the pane to an ordinary shell prompt. Full-screen applications, password
or host-key prompts, and uncertain shell boundaries block non-interactive agent
commands to avoid sending input to the wrong foreground program. Confirm that
`$SHELL` is usable or that `/bin/sh` is available, then inspect `/status` and
the pane-local readiness diagnostic. Do not use a readiness override merely to
avoid an unexplained boundary failure.

## Provider, authentication, or MCP is unavailable

Run `mez auth status` and verify the selected model with `/model`. Complete the
reported sign-in action rather than treating a partial browser or device flow
as authenticated. For MCP, use `/list-mcp` to inspect a server's enabled,
unavailable, or session-blacklisted state; fix its configured executable,
endpoint, credential reference, or network reachability before retrying.

Provider SSE streams require a recognized terminal event such as
`response.completed`, `response.failed`, `response.incomplete`, `[DONE]`, or
Anthropic `message_stop`. If the HTTP peer closes first, Mez treats the EOF as
a retryable transport interruption, discards provisional streamed output, and
uses the existing bounded actor-owned retry policy. Repeated premature EOF
usually indicates an unstable provider, proxy, or network path rather than a
malformed model response.

## Project behavior is missing or blocked

Check `mez sandbox trust list` and inspect the project's `.mezzanine` overlay
and `AGENTS.md`. A pending or rejected project overlay is intentionally not
applied. Trusting it enables eligible project configuration but does not grant
approval, host access, or additional sandbox authority.

## Display, width, or copy behavior is wrong

Check the configured terminal profile and `terminal.emoji_width`; use `wide`
or `narrow` to match the host terminal's emoji-cell behavior. Alternate-screen
applications do not contribute normal scrollback, so their copy behavior is
limited to visible content. Consult the terminal reference for supported
compatibility behavior before changing passthrough or profile settings.

## A local attachment refreshes only after input or focus

The built-in Unix attach client uses the standard event socket derived from the
control socket (for example, `session.sock` and `session.event.sock`) only as a
redraw wakeup channel; it still fetches each rendered view over control. If the
event socket is missing or refuses the connection, attachment remains usable,
but an idle client can wait until keyboard, mouse, focus, or resize activity
causes another control step.

For `mez serve`, keep the default auxiliary sockets enabled when using the
built-in attach client. `--no-aux-sockets` intentionally disables this wakeup,
and a custom `--event-socket` path is not discovered by `mez attach`. Verify
that the standard derived event socket exists and is owned by the same user as
the control socket.

Hosted sessions created by `mez host serve` currently do not publish that
derived event socket. Direct Unix attachment to one of those session sockets
therefore has this idle-redraw limitation; Iroh attachment uses its independent
event stream. Until hosted event-socket publication is corrected, use Iroh or a
regular `mez serve` session when event-driven idle redraws are required.

## A remote X11 application does not open locally

Confirm all three opt-ins: the host has `transport.iroh.x11.enabled = true`,
the attachment is an authenticated Iroh primary, and the client used `--x11`
or `--x11-trusted`. Unix and observer attachments cannot own an X11 route. A
legacy peer that does not advertise `x11_forwarding` fails visibly rather than
continuing without forwarding.

On the attaching machine, check that `DISPLAY` names a local Unix socket,
constrained XQuartz launchd socket, or numeric loopback TCP display; remote
hostnames are rejected. Check that `XAUTHORITY` (or the default authority file)
is owner-private, contains an exact `MIT-MAGIC-COOKIE-1` record for that
display, and that `xauth` is installed. `--x11` additionally requires working
X SECURITY untrusted-cookie generation. If that operation fails, fix the
local X server or use no forwarding; do not expect or script a fallback to
trusted mode. `--x11-trusted` also requires the host's explicit
`allow_trusted` policy.

Only one attachment owns a session's X11 route. A conflict means another
primary currently owns it. Use `--x11-takeover` only for an intentional
replacement. Takeover, detach, transport loss, trust or lease revocation, and
session shutdown close existing streams. After reattach, restart or reconnect
the GUI application so it opens against the new route credentials.

Run local `mez remote status` or `show-metrics` and inspect only the aggregate
`x11`/`[x11 forwarding]` counters: route activity, accepted or rejected
sockets, active streams, and stream outcomes. A rising
`sockets_rejected_no_route` count indicates no published owner;
`sockets_rejected_capacity` indicates the configured per-route cap;
`streams_cancelled` covers detach, takeover, revocation, and shutdown of an
owned route; `streams_failed` covers malformed credentials, setup/connect
timeout, and transport failures. Every started stream contributes to exactly
one of completed, cancelled, or failed. These diagnostics intentionally omit
cookies, route tokens, local display targets, authority paths, and X11 bytes.
`authority_repair_pending = true` means logical route ownership has already
been revoked but the private Xauthority file could not be atomically replaced;
repair the session Xauthority directory or storage. A later successful route
publication clears the flag, while `authority_publication_failures` retains the
aggregate failure count.
If `x11.restart_pending` is true, compare `x11.applied` with
`x11.configured`; the applied values govern current route admission. Restart
the daemon to converge them. Reload never enables, disables, or weakens an
existing session proxy in place.

X11/Xwayland socket applications are in scope. Native Wayland, audio, D-Bus,
portals, device forwarding, network-transparent MIT-SHM, and dependable direct
GLX/DRI acceleration are not. Prefer software rendering when an application's
graphics path assumes local shared memory or devices.

## Iroh compression is unavailable or inefficient

Run `show-iroh-status` from the affected remote client. `Codec unavailable`
means that client has no correlated live Iroh connection; `insufficient sample`
means the current connection/codec has not yet carried a complete frame. The
reported compression ratio, bytes saved or expanded, and frame counts aggregate
the active connection's lifetime and reset on reconnect or codec change.
An ALPN failure usually means the peers have no mutually configured codec; keep
`none` in the preference list during mixed-version rollout. Malformed envelope,
decoded-size, or unsupported-codec failures close only that connection; retain
the non-sensitive failure class and do not log payloads or credentials.

For high CPU, compare zstd with LZ4 using `just iroh-compression-bench`. For a
poor ratio or expansion, confirm the workload is above the configured threshold
and actually compressible before lowering `compression_min_bytes`. Immediate
rollback is `compression_codecs = ["none"]` followed by daemon restart. Codec
choice and compression ratio do not change direct/relay path quality.

## An Iroh copy does not reach the attaching machine clipboard

Confirm the attachment is a primary and negotiated event-stream version 2 with
the explicit `client_clipboard_write` capability. Observers, version-1 peers,
and legacy fallback sessions intentionally retain server-only copy behavior.
The attaching client, not the server, selects `terminal.clipboard_copy_command`;
verify that the local command exists and can access the client desktop session.
Linux clients try `wl-copy`, `xclip`, and `xsel` by default, while macOS clients
try `pbcopy`. WSL clients first try Windows PowerShell `Set-Clipboard` with
explicit UTF-8 stdin decoding so the destination is the Windows host clipboard.
If Windows executable interoperability is disabled, configure an explicit
`terminal.clipboard_copy_command` or restore interoperability. Headless sessions
may have no usable provider.

Clipboard writes are best-effort and bounded to 8 MiB. A local provider failure
does not undo the server internal paste buffer or server-host clipboard attempt,
and it does not interrupt rendering or input. Troubleshoot with payload-free
version, capability, provider availability, and failure-class information;
never copy clipboard contents into logs or diagnostics. Clipboard reads and
remote paste are not part of the Iroh client clipboard feature.

## Related pages

- [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md)
- [Configuration](../configuration/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

If these checks do not identify a safe remediation, preserve the bounded
diagnostics and report the observed symptom, command, and result to the
maintainer or operator responsible for the affected environment.
