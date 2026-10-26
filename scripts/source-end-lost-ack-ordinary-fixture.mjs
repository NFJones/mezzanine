/** Installed ordinary native producer: Rust withholds one real durable end ACK
 * AFTER actor/store proof, then closes that connection. Only the original end
 * can reconnect; no enrollment, callback, usage or provider work is replayed. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 20000);
const { createPersistentTelemetryClient } = await import(pathToFileURL(process.argv[2]).href);
const { peerHelper } = await import(pathToFileURL(process.argv[3]).href);
const client = createPersistentTelemetryClient({ harness: "pi", session: "end-lost-ack-session",
  version: "fixture", displayName: "ordinary end ACK fixture", instance: "end-lost-ack-instance", peerHelper });
try {
  assert.equal(await client.start(), true);
  assert.equal((await client.presentation("complete")).delivered, true);
  const [ended, duplicate] = await Promise.all([client.end(), client.end()]);
  assert.equal(ended.delivered, true);
  assert.equal(ended.durable, true);
  assert.deepEqual(duplicate, ended);
  assert.deepEqual(await client.end(), ended);
  assert.equal(client.status().sourceEnd, "durable");
  assert.equal(await client.start(), false);
  assert.equal((await client.presentation("running")).delivered, false);
  assert.equal(client.successor("forbidden-successor"), undefined);
} finally { client.detach(); clearTimeout(lifetime); }
