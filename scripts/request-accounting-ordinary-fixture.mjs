/** Installed ordinary-process request accounting acceptance. Standard MEZ
 * discovery only; no injected credentials, observer fd3, vendor or provider.
 * Simulated incurred counters are reported only after real native readiness. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
const lifetime = setTimeout(() => { process.exitCode = 1; }, 20000);
const { createPersistentTelemetryClient } = await import(pathToFileURL(process.argv[2]).href);
const { peerHelper } = await import(pathToFileURL(process.argv[3]).href);
const client = createPersistentTelemetryClient({ harness: "pi", session: "session-a", version: "fixture",
  displayName: "ordinary request fixture", instance: "request-instance-a", peerHelper });
try {
  assert.equal(await client.start(), true);
  assert.equal((await client.presentation("running")).delivered, true);
  const ticket = await client.beginRequest("a".repeat(64));
  assert(ticket, "native original request marker unavailable");
  const report = { event_id: "b".repeat(64), observed_at: Math.floor(Date.now() / 1000), provider: "fixture", model: "model",
    counters: { input_tokens: 10, output_tokens: 3, cached_input_tokens: 2 } };
  const ack = await client.endRequest(ticket, report);
  assert.equal(ack.delivered, true); assert.equal(ack.durable, true); assert.equal(ack.applied, true);
  assert.equal((await client.endRequest(ticket, report)).durable, true);
  const changed = { ...report, counters: { ...report.counters, input_tokens: 11 } };
  assert.equal((await client.endRequest(ticket, changed)).durable, false);
  assert.equal(client.status().usage, "request-bound-external-reported");
  assert.equal((await client.presentation("complete")).delivered, true);
  assert.equal((await client.end()).delivered, true);
} finally { client.detach(); clearTimeout(lifetime); }
