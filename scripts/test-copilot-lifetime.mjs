/** Offline pulse-loop conservation tests with an injected clock/transport.
 * No daemon, vendor, provider or real ten-second wait is required. Native proof
 * and installed periodic reconnection are separately qualified in Rust fixtures.
 */
import assert from "node:assert/strict";
import test from "node:test";
import { createCopilotPulseLifetime } from "../crates/mezzanine/src/integrations/bootstrap/copilot_lifetime.mjs";

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
