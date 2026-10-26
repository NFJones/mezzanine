/** Actual installed ordinary Node producer; Rust withholds a real committed ACK
 * then replaces the entire control owner/reopens its store. No namespace, launch
 * credential, fd3, native proof or provider invocation is supplied by this test. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 20000);
const { createPersistentTelemetryClient } = await import(pathToFileURL(process.argv[2]).href);
const { peerHelper } = await import(pathToFileURL(process.argv[3]).href);
const client = createPersistentTelemetryClient({ harness: "pi", session: "cold-accounting-session", version: "fixture",
  displayName: "ordinary cold accounting fixture", instance: "cold-accounting-instance", peerHelper });
try {
  assert.equal(await client.start(), true);
  const ticket = await client.beginRequest("a".repeat(64));
  assert(ticket, "original native request readiness unavailable");
  const report = { event_id: "b".repeat(64), observed_at: Math.floor(Date.now() / 1000),
    provider: "fixture", model: "original-model", counters: { input_tokens: 10, output_tokens: 3, cached_input_tokens: 2 } };
  const ack = await client.endRequest(ticket, report);
  assert.equal(ack.delivered, true); assert.equal(ack.durable, true);
  assert.equal(ack.applied, false, "actual committed report must dedup after native cold recovery");
  assert.equal(client.status().epoch, 1);
  const distinct = await client.beginRequest("c".repeat(64)); assert(distinct);
  const next = await client.endRequest(distinct, { event_id: "d".repeat(64), observed_at: Math.floor(Date.now() / 1000),
    provider: "fixture", model: "original-model", counters: { input_tokens: 7, output_tokens: 2, cached_input_tokens: 1 } });
  assert.equal(next.durable, true); assert.equal(next.applied, true);
  assert.equal((await client.presentation("complete")).delivered, true);
  assert.equal((await client.end()).durable, true);
} finally { client.detach(); clearTimeout(lifetime); }
