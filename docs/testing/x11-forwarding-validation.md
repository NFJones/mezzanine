# X11 forwarding validation

## Purpose

Validate Mezzanine's complete X11 socket-forwarding path and keep Linux Xvfb
and macOS XQuartz evidence separate from component tests. This feature forwards
X11 protocol bytes; it does not render windows itself.

## Automated coverage

The normal Rust suite runs an in-process fake-X-server regression that composes
the stable session proxy, fake-cookie setup, exact server-opened Iroh stream,
generation/token preface validation, client-local fake-to-real cookie rewrite,
frozen local target, setup reply, and later bidirectional bytes. Focused proxy
tests cover both setup byte orders, malformed setup, no-route rejection,
takeover and stale cleanup, idle established streams, cancellation, setup
timeout isolation, route capacity, and permit recovery. Compression tests prove
fake cookies and route tokens use identity/reset framing; raw X11 bytes never
enter control or event framing.

Run the focused integrated test with:

```console
cargo test --quiet -p mezzanine \
  cli::control_client::tests::x11_proxy_iroh_client_and_fake_server_complete_one_round_trip
```

## Linux Xvfb acceptance

CI installs `xvfb` and `xauth`, starts an owner-private authenticated Xvfb
display, and invokes:

```console
sh scripts/test-x11-forwarding.sh
```

The script runs the same proxy/Iroh/client path twice: trusted mode using the
selected real cookie, then untrusted mode using `xauth generate ... untrusted`.
It requires the X SECURITY operation to succeed and never retries in trusted
mode. A setup success reply from Xvfb must be observed in each mode. The script
uses a bounded timeout and cleans up Xvfb and its temporary authority database.

For a local Linux run, install Xvfb and xauth first. Missing prerequisites are
a failed acceptance run, not a passing skip. If display 99 is occupied, choose
another free number with `MEZ_X11_TEST_DISPLAY`.

## Manual XQuartz checklist

Run this matrix on a macOS machine with a current XQuartz installation. Record
the XQuartz version, macOS version, application, result, and any limitation.

1. Start XQuartz and confirm its launchd-style `DISPLAY` resolves to the local
   XQuartz socket and has a matching `MIT-MAGIC-COOKIE-1` record.
2. Pair an authenticated primary with an X11-enabled host.
3. Attach with `--x11`; verify untrusted credential generation succeeds or
   record the explicit X SECURITY failure. Do not substitute trusted mode for
   a failed untrusted result.
4. Launch one representative Xlib, GTK, or Qt program in the remote pane and
   verify its window appears locally and remains responsive alongside terminal
   input and redraws.
5. Detach and verify the window's connection closes. Reattach and verify a new
   application connection succeeds while the remote `DISPLAY` and `XAUTHORITY`
   paths remain stable.
6. Create a competing primary route, verify conflict, then use
   `--x11-takeover` and verify the old application's stream closes before a new
   application connects.
7. With host `allow_trusted = true`, repeat with `--x11-trusted`. With it false,
   verify trusted initialization is denied.
8. Revoke device trust or the active lease and verify the route and streams
   close promptly while local Unix administration remains available.

## Privacy and compatibility checks

Inspect failures and `remote/status` or `show-metrics` output. They may contain
only reason classes and aggregate route/socket/stream counters. They must not
contain the real or fake cookie, route token, local display target, local
authority path, or raw X11 payload.

The acceptance target is conventional X11 and Xwayland socket traffic. Native
Wayland, audio, D-Bus, portals, device forwarding, cross-network MIT-SHM, and
dependable direct GLX/DRI are outside the forwarding contract. Record such
application requirements as compatibility limitations rather than weakening
target validation or credential policy.
