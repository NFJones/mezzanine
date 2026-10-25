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
let successor;
try {
  assert.equal(await client.start(), true);
  const originalRun = client.status().run;
  assert(Number.isSafeInteger(originalRun) && originalRun > 0);
  assert.equal(client.status().epoch, 1);
  assert.equal((await client.presentation("running")).delivered, true);
  const ticket = await client.beginRequest("a".repeat(64));
  assert(ticket, "native original request marker unavailable");
  const report = { event_id: "b".repeat(64), observed_at: Math.floor(Date.now() / 1000), provider: "fixture", model: "model",
    counters: { input_tokens: 10, output_tokens: 3, cached_input_tokens: 2 } };
  const ack = await client.endRequest(ticket, report);
  assert.equal(ack.delivered, true); assert.equal(ack.durable, true); assert.equal(ack.applied, true);
  assert.equal((await client.endRequest(ticket, report)).durable, true);
  successor = client.successor("request-instance-b");
  assert.equal(await successor.start(), true, "accounting observer handoff unavailable");
  assert.equal(successor.status().run, originalRun);
  assert.equal(successor.status().epoch, 2);
  client.detach();
  assert.equal((await successor.endRequest(ticket, report)).durable, false,
    "tickets must remain client-instance-local");
  const restored = await successor.beginRequest("a".repeat(64));
  assert(restored, "original request readiness unavailable after observer replacement");
  for (const field of ["request_id", "admitted_at", "source_witness", "origin"]) {
    assert.equal(restored[field] === ticket[field], true, `original ${field} must survive handoff`);
  }
  const replay = await successor.endRequest(restored, report);
  assert.equal(replay.delivered, true); assert.equal(replay.durable, true);
  assert.equal(replay.applied, false, "fresh successor replay must not recharge expense");
  const changed = { ...report, counters: { ...report.counters, input_tokens: 11 } };
  const conflictTicket = await successor.beginRequest("a".repeat(64));
  assert(conflictTicket, "fresh conflict ticket unavailable");
  assert.equal((await successor.endRequest(conflictTicket, changed)).durable, false,
    "ledger must reject changed finalized counters beyond client-local cache");
  successor.abandonRequest(conflictTicket);
  assert.equal(successor.status().usage, "request-bound-external-reported");
  assert.equal((await successor.presentation("complete")).delivered, true);
  assert.equal((await successor.end()).delivered, true);
} finally { client.detach(); successor?.detach(); clearTimeout(lifetime); }
