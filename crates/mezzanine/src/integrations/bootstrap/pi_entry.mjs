/**
 * Best-effort Pi directory-extension entry point.
 *
 * Only an explicitly selected session and inherited observation socket can
 * enable this observer. Environment markers are discovery hints, not daemon
 * credentials or authority. Factory loading opens no descriptor or timer.
 * A private launcher must supply descriptor 3; missing, stale or duplicate
 * binding stays neutral. Same-session reload leases the process-owned stream
 * once the old extension releases it; daemon capabilities never cross this link.
 */
import { Socket } from "node:net";
import { fstatSync } from "node:fs";
import { createPiStreamExtension } from "./pi_extension.mjs";
import { createPiBindingExtension } from "./pi_binding.mjs";

const channelOwner = Symbol.for("mezzanine.pi.observer-descriptor.v1");

/** Acquire one instance lease from a process-owned observation channel. Reload
 * releases only that lease; the fd and neutral error handler survive. Duplicate
 * loads cannot borrow an active channel, and a closed channel never reopens. */
export function acquireProcessObserverChannel(state, openStream, startReason) {
  if (state.closed || state.lease) throw new Error("Pi observer channel unavailable");
  if (startReason !== undefined && ((!state.stream && startReason !== "startup")
      || (state.stream && state.transition !== startReason))) throw new Error("Pi observer transition unavailable");
  if (!state.stream) {
    state.stream = openStream();
    state.stream.on("error", () => { state.closed = true; });
    state.stream.on("close", () => { state.closed = true; });
  }
  const lease = {};
  const epoch = (state.epoch ?? 0) + 1;
  if (!Number.isSafeInteger(epoch)) throw new Error("Pi observer epoch exhausted");
  state.epoch = epoch;
  state.lease = lease;
  state.transition = undefined;
  return { stream: state.stream, epoch, persistent: true, fail() {
    if (state.lease !== lease) return;
    state.closed = true;
    try { state.stream.end(); } catch { /* no vendor behavior change */ }
  }, close(reason) {
    if (state.lease !== lease) return;
    state.lease = undefined;
    state.transition = reason;
    if (!["reload", "new", "resume", "fork"].includes(reason)) { state.closed = true; state.stream.end(); }
  } };
}

/** Registers an explicitly bound observer without opening its stream. */
export function registerInheritedObserver(pi, binding, open) {
  if (binding?.descriptor !== "3" || typeof binding.session !== "string"
      || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,126}[A-Za-z0-9]$|^[A-Za-z0-9]$/.test(binding.session)
      || typeof open !== "function") return;
  createPiStreamExtension(binding.session, open)(pi);
}

/** Released-loader entry: missing launch markers leave the extension inert. */
export default function mezzaninePiObserver(pi) {
  if (process.env.MEZ_PI_OBSERVER_PROTOCOL === "2") {
    const binding = { descriptor: process.env.MEZ_PI_OBSERVER_FD, session: process.env.MEZ_PI_OBSERVER_SESSION };
    if (binding.descriptor !== "3" || typeof binding.session !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(binding.session)) return;
    createPiBindingExtension(binding.session, (reason) => {
      const state = globalThis[channelOwner] ??= {};
      return acquireProcessObserverChannel(state, () => {
        if (!fstatSync(3).isSocket()) throw new Error("Pi observer channel unavailable");
        return new Socket({ fd: 3, readable: false, writable: true });
      }, reason);
    })(pi);
    return;
  }
  registerInheritedObserver(pi, {
    descriptor: process.env.MEZ_PI_OBSERVER_FD,
    session: process.env.MEZ_PI_OBSERVER_SESSION,
  }, () => {
    const state = globalThis[channelOwner] ??= {};
    return acquireProcessObserverChannel(state, () => {
      if (!fstatSync(3).isSocket()) throw new Error("Pi observer channel unavailable");
      return new Socket({ fd: 3, readable: false, writable: true });
    });
  });
}
