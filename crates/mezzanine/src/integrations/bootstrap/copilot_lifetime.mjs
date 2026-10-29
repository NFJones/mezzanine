/** Bounded original Copilot startup attachment and concrete pulse lifetimes.
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

/** Wait only for an existing original enrollment to become available at startup.
 * The fixed read-only attempt never enrolls, supplies health or invokes a vendor.
 * At most32 serialized attempts start in15seconds, spaced500ms after failure;
 * each transport separately caps itself at3seconds. Stop fences an in-flight
 * result and wakes a pending delay. No retry is possible after first success.
 */
export function createCopilotStartupAttachment({ attempt, now, setTimeout, clearTimeout } = {}) {
  let stopped = false, timer, wake;
  const stop = () => {
    if (stopped) return;
    stopped = true;
    try { if (timer !== undefined) clearTimeout(timer); } catch { /* neutral teardown */ }
    wake?.();
  };
  const ready = (async () => {
    try {
      if ([attempt, now, setTimeout, clearTimeout].some(value => typeof value !== "function")) return;
      const started = now();
      for (let count = 0; !stopped && count < 32 && now() - started < 15000; count++) {
        let original;
        try { original = await attempt(); } catch { /* bounded unavailable observation */ }
        if (stopped) return;
        if (original) return original;
        if (count === 31 || now() - started >= 15000) return;
        await new Promise(resolve => {
          wake = resolve;
          // This finite startup operation owns its delay. Unlike ongoing
          // pulses, it must finish even when the SDK unrefs its transport.
          timer = setTimeout(resolve, Math.min(500, 15000 - (now() - started)));
        });
        wake = undefined;
        timer = undefined;
      }
    } catch { /* missing clock/timer remains unavailable */ }
    finally { stop(); }
  })();
  return Object.freeze({ stop, ready });
}
