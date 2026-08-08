# Troubleshooting

## Purpose

Diagnose common Mezzanine symptoms with bounded checks, then follow the
canonical owner for configuration, safety, provider, or terminal remediation.

## Prerequisites

Identify the affected pane or session and preserve any useful diagnostic output
before restarting or changing policy.

## Session cannot be found or reattached

Run `mez list`, then select the intended session with `mez attach <session-id>`
or select its control socket with `-S` or `-L`. A live primary-client conflict
requires detaching or transferring the existing primary rather than attaching a
second primary. If a background daemon started by `mez new` failed, inspect
its private `<control-socket>.diagnostics.log` before creating a replacement session. A
foreground `mez serve` reports its diagnostics to the terminal that started it.

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

## Related pages

- [Lifecycle, detach, and recovery](lifecycle-detach-and-recovery.md)
- [Configuration](../configuration/README.md)
- [Safety, trust, and security](../safety-and-trust/README.md)
- [Manual reference](../reference-manual/README.md)

## Next step

If these checks do not identify a safe remediation, preserve the bounded
diagnostics and report the observed symptom, command, and result to the
maintainer or operator responsible for the affected environment.
