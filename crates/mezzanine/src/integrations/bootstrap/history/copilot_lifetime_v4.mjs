/** Bounded concrete Copilot pulse lifetime with injected fixed transport/clock.
 * No vendor API, process, socket or timer is touched on import. One pulse may be
 * outstanding; lost ACKs retry the same counter and cannot assert fresh health.
 * Stop permanently fences late replies/ticks and never enrolls a replacement.
 */

/** Start one original instance's ten-second proof loop. The caller owns nonce
 * generation and a fixed native transport; no event can choose RPC semantics.
 */
export function createCopilotPulseLifetime({ send, setInterval, clearInterval } = {}) {
  let stopped = false, busy = false, sequence = 1, timer;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    try { if (timer !== undefined) clearInterval(timer); } catch { /* neutral teardown */ }
  };
  const tick = async () => {
    if (stopped || busy) return false;
    busy = true;
    try {
      const reply = await send(sequence);
      if (stopped) return false;
      if (reply?.observed === false && reply.terminal === true) { stop(); return false; }
      if (reply?.observed !== true || reply.sequence !== sequence || typeof reply.changed !== "boolean") return false;
      if (!Number.isSafeInteger(sequence + 1)) { stop(); return false; }
      sequence++;
      return true;
    } catch { return false; }
    finally { busy = false; }
  };
  let ready;
  if (typeof send !== "function" || typeof setInterval !== "function" || typeof clearInterval !== "function") {
    stop(); ready = Promise.resolve(false);
  } else {
    try {
      timer = setInterval(() => { void tick(); }, 10000);
      timer?.unref?.();
      ready = tick();
    } catch { stop(); ready = Promise.resolve(false); }
  }
  return Object.freeze({ stop, ready });
}
