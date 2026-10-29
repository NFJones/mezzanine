/** Offline pulse-loop conservation tests with an injected clock/transport.
 * No daemon, vendor, provider or real ten-second wait is required. Native proof
 * and installed periodic reconnection are separately qualified in Rust fixtures.
 */
import assert from "node:assert/strict";
import test from "node:test";
import { createCopilotPulseLifetime, createCopilotStartupAttachment } from "../crates/mezzanine/src/integrations/bootstrap/copilot_lifetime.mjs";

/** One owned fake timer records actual setup/teardown, without changing clocks. */
function clock() {
  let callback, cleared = 0, unreferenced = 0;
  const token = { unref() { unreferenced++; } };
  return {
    setInterval(fn, interval) { assert.equal(interval, 10000); callback = fn; return token; },
    clearInterval(value) { assert.equal(value, token); cleared++; },
    tick() { callback(); },
    get cleared() { return cleared; }, get unreferenced() { return unreferenced; },
  };
}
const flush = () => new Promise(resolve => setImmediate(resolve));

/** No concurrent tick may bypass an outstanding pulse. Lost ACKs resend the
 * same counter; an actual duplicate ACK may advance, without inventing freshness.
 */
test("Copilot pulses serialize and retain the original counter after ACK loss", async () => {
  const c = clock(), sent = [], replies = [];
  const lifetime = createCopilotPulseLifetime({ ...c, send: sequence => {
    sent.push(sequence); return new Promise(resolve => replies.push(resolve));
  } });
  assert.equal(c.unreferenced, 1);
  c.tick(); c.tick(); assert.deepEqual(sent, [1]);
  replies.shift()({ observed: true, sequence: 1, changed: true });
  assert.equal(await lifetime.ready, true);
  c.tick(); replies.shift()(undefined); await flush();
  c.tick(); assert.deepEqual(sent, [1, 2, 2]);
  replies.shift()({ observed: true, sequence: 2, changed: false }); await flush();
  c.tick(); assert.deepEqual(sent, [1, 2, 2, 3]);
  lifetime.stop(); replies.shift()({ observed: true, sequence: 3, changed: true }); await flush();
  c.tick(); assert.deepEqual(sent, [1, 2, 2, 3]); assert.equal(c.cleared, 1);
});

/** Stop is permanent even when an old transport returns a valid-looking ACK.
 * Repeated stop/ticks cannot create timers, advance identity or restart delivery.
 */
test("Copilot pulse stop fences late ACKs and queued ticks", async () => {
  const c = clock(); let resolve, calls = 0;
  const lifetime = createCopilotPulseLifetime({ ...c, send: () => {
    calls++; return new Promise(done => { resolve = done; });
  } });
  lifetime.stop(); lifetime.stop();
  resolve({ observed: true, sequence: 1, changed: true });
  assert.equal(await lifetime.ready, false);
  c.tick(); await flush(); assert.equal(calls, 1); assert.equal(c.cleared, 1);
});

/** A typed terminal refusal closes this local owner without reattachment.
 * Exceptions and malformed ACKs instead retain the same bounded original pulse.
 */
test("Copilot pulse terminal refusal stops while transient loss retains counter", async () => {
  const c = clock(), calls = [];
  let reply;
  const lifetime = createCopilotPulseLifetime({ ...c, send: async sequence => {
    calls.push(sequence); if (calls.length === 1) throw Error("PRIVATE"); return reply;
  } });
  assert.equal(await lifetime.ready, false);
  reply = { observed: true, sequence: 99, changed: true }; c.tick(); await flush();
  reply = { observed: true, sequence: 1, changed: true }; c.tick(); await flush();
  reply = { observed: false, terminal: true }; c.tick(); await flush();
  c.tick(); await flush(); assert.deepEqual(calls, [1, 1, 1, 2]); assert.equal(c.cleared, 1);
});

/** Missing code-owned dependencies are unavailable and do no work on load. */
test("Copilot pulse loop refuses missing dependencies", async () => {
  const lifetime = createCopilotPulseLifetime();
  assert.equal(await lifetime.ready, false); lifetime.stop();
});

/** A controllable startup clock owns one pending delay, with no real wait. */
function startupClock() {
  let time = 0, pending, cleared = 0, unreferenced = 0;
  return {
    now: () => time,
    setTimeout(fn, delay) {
      assert(delay > 0 && delay <= 500);
      assert.equal(pending, undefined);
      pending = { fn, delay, unref() { unreferenced++; } };
      return pending;
    },
    clearTimeout(timer) { assert.equal(timer, pending); pending = undefined; cleared++; },
    tick(advance = true) { const timer = pending; assert(timer); pending = undefined;
      if (advance) time += timer.delay; timer.fn(); },
    expire() { time = 15000; },
    get pending() { return pending; }, get cleared() { return cleared; },
    get unreferenced() { return unreferenced; },
  };
}

/** Enrollment may arrive after extension loading. Only the first successful
 * fixed read-only result is retained; retries themselves supply no idle proof.
 */
test("Copilot startup waits for original enrollment without polling after attachment", async () => {
  const c = startupClock(), original = Object.freeze({ original: true }); let calls = 0;
  const startup = createCopilotStartupAttachment({ ...c, attempt: async () => {
    calls++; if (calls === 1) throw Error("PRIVATE"); return calls === 3 ? original : undefined;
  } });
  await flush(); assert.equal(calls, 1); assert.equal(c.unreferenced, 0);
  c.tick(); await flush(); assert.equal(calls, 2);
  c.tick(); assert.equal(await startup.ready, original); assert.equal(calls, 3);
  assert.equal(c.pending, undefined); startup.stop(); assert.equal(calls, 3);
});

/** Shutdown fences late successful native replies and cancels/wakes a retry
 * delay. Neither path may attach or start another attempt after teardown.
 */
test("Copilot startup shutdown fences pending transport and delayed retries", async () => {
  const c = startupClock(); let resolve, calls = 0;
  const startup = createCopilotStartupAttachment({ ...c, attempt: () => {
    calls++; return new Promise(done => { resolve = done; });
  } });
  startup.stop(); resolve({ original: true });
  assert.equal(await startup.ready, undefined); assert.equal(calls, 1);
  const delayed = createCopilotStartupAttachment({ ...c, attempt: async () => { calls++; } });
  await flush(); assert(c.pending); delayed.stop(); delayed.stop();
  assert.equal(await delayed.ready, undefined); assert.equal(c.pending, undefined);
  assert.equal(c.cleared, 1); assert.equal(calls, 2);
});

/** The monotonic startup deadline and independent attempt cap both terminate
 * unavailable observation; neither sleeping nor clock stalls can make it endless.
 */
test("Copilot startup enforces both deadline and finite attempt budget", async () => {
  const c = startupClock(); let calls = 0;
  const startup = createCopilotStartupAttachment({ ...c, attempt: async () => { calls++; } });
  await flush(); c.expire(); c.tick(false);
  assert.equal(await startup.ready, undefined); assert.equal(calls, 1);
  const frozen = startupClock(); calls = 0;
  const capped = createCopilotStartupAttachment({ ...frozen, attempt: async () => { calls++; } });
  for (let i = 0; i < 31; i++) { await flush(); frozen.tick(false); }
  assert.equal(await capped.ready, undefined); assert.equal(calls, 32);
  assert.equal(frozen.pending, undefined);
  assert.equal(await createCopilotStartupAttachment().ready, undefined);
});
