/**
 * Candidate Pi 1.0.2 directory-extension entry point.
 *
 * Only an explicitly selected session and inherited observation socket can
 * enable this observer. Environment markers are discovery hints, not daemon
 * credentials or authority. Factory loading opens no descriptor or timer.
 * A private launcher must supply descriptor 3; missing, stale or duplicate
 * binding stays neutral. Reload needs a new launcher-owned observer channel.
 * This artifact is not enabled in the certified bootstrap registry.
 */
import { Socket } from "node:net";
import { fstatSync } from "node:fs";
import { createPiStreamExtension } from "./pi_extension.mjs";

const channelOwner = Symbol.for("mezzanine.pi.observer-descriptor.v1");

/** Registers an explicitly bound observer without opening its stream. */
export function registerInheritedObserver(pi, binding, open) {
  if (binding?.descriptor !== "3" || typeof binding.session !== "string"
      || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,126}[A-Za-z0-9]$|^[A-Za-z0-9]$/.test(binding.session)
      || typeof open !== "function") return;
  createPiStreamExtension(binding.session, open)(pi);
}

/** Released-loader entry: missing launch markers leave the extension inert. */
export default function mezzaninePiObserver(pi) {
  registerInheritedObserver(pi, {
    descriptor: process.env.MEZ_PI_OBSERVER_FD,
    session: process.env.MEZ_PI_OBSERVER_SESSION,
  }, () => {
    // Process-local duplicate loads may register callbacks but cannot wrap the
    // same descriptor twice. Closed channels are not silently reopened/rebound.
    if (globalThis[channelOwner] || !fstatSync(3).isSocket()) {
      throw new Error("Pi observer channel unavailable");
    }
    globalThis[channelOwner] = true;
    const stream = new Socket({ fd: 3, readable: false, writable: true });
    return { stream, close() { stream.end(); } };
  });
}
