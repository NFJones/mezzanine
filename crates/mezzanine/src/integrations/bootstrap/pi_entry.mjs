/**
 * Best-effort Pi directory-extension entry point.
 *
 * Ordinary callback context selects the real session; native-qualified daemon
 * enrollment supplies observational authority. Factory loading opens no socket
 * or timer and needs no vendor FD3, launcher markers or preselected session.
 * Legacy stream utility exports remain isolated fixtures, not default activation.
 */
import { createPiStreamExtension } from "./pi_extension.mjs";
import { registerOrdinaryPiObserver } from "./pi_persistent.mjs";

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

/** Ordinary released-loader entry; standard route hints alone grant no authority. */
export default function mezzaninePiObserver(pi) {
  registerOrdinaryPiObserver(pi);
}
