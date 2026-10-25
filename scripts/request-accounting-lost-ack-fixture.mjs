/** Real installed ordinary-process accounting under commit-before-reply loss.
 * Rust withholds one actually committed ACK and closes the socket. The same
 * native producer retains its ticket/report; it never repeats vendor work or
 * obtains another source. No injected token, observer fd3 or provider is used. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 20000);
const { createPersistentTelemetryClient } = await import(pathToFileURL(process.argv[2]).href);
const { peerHelper } = await import(pathToFileURL(process.argv[3]).href);
const client = createPersistentTelemetryClient({ harness: "pi", session: "lost-ack-session", version: "fixture",
  displayName: "ordinary lost ACK fixture", instance: "lost-ack-instance", peerHelper });
try {
  assert.equal(await client.start(), true);
  const original = client.status();
  assert(Number.isSafeInteger(original.run) && original.run > 0);
  assert.equal(original.epoch, 1);
  const ticket = await client.beginRequest("a".repeat(64));
  assert(ticket, "native original request readiness unavailable");
  const report = { event_id: "b".repeat(64), observed_at: Math.floor(Date.now() / 1000),
    provider: "fixture", model: "model", counters: { input_tokens: 10, output_tokens: 3, cached_input_tokens: 2 } };
  const ack = await client.endRequest(ticket, report);
  assert.equal(ack.delivered, true);
  assert.equal(ack.durable, true);
  assert.equal(ack.applied, false, "lost first ACK must recover the committed duplicate");
  assert.equal(client.status().run, original.run);
  assert.equal(client.status().epoch, original.epoch);
  assert.equal(client.status().usage, "request-bound-external-reported");
  assert.equal((await client.presentation("complete")).delivered, true);
  assert.equal((await client.end()).delivered, true);
} finally {
  client.detach();
  clearTimeout(lifetime);
}
