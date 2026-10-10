/** Offline original-observer SDK accounting tests. Injected runner has no
 * vendor/provider/socket access; native billing proof is tested separately. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { createCuratedRequestAccountingClient } from "../crates/mezzanine/src/integrations/bootstrap/curated_request_accounting.mjs";

const observer = () => ({ external_session_id: "session-a", generation: 7,
  observer_witness: "a".repeat(64), run_id: 1, observer_epoch: 1, agent_id: "external-a" });
const marker = "b".repeat(64);
const report = () => ({ event_id: "c".repeat(64), observed_at: 101, provider: "fixture", model: "model",
  counters: { input_tokens: 10, output_tokens: 3 } });
const ready = () => ({ protocol: "external-request/1", accepted: true, request_id: marker,
  admitted_at: 100, source_witness: "d".repeat(64), origin: "unattributed", controls: [] });
const ack = () => ({ accepted: true, durable: true, applied: false, revision: 1,
  coverage: { source: "external-reported", reasoning_known: false } });

/** Only the original captured public selectors may enter fixed capsules;
 * response loss retries the same normalized observation, never vendor work. */
test("curated accounting freezes original observer and retries immutable reports", async () => {
  const source = observer(), calls = [];
  let ends = 0;
  const client = createCuratedRequestAccountingClient({ observer: source, isCurrent: () => true,
    send: async capsule => {
      calls.push(capsule);
      return capsule.operation.endsWith("begin") ? JSON.stringify(ready())
        : ++ends === 1 ? undefined : JSON.stringify(ack());
    } });
  Object.assign(source, { generation: 99, observer_witness: "f".repeat(64), external_session_id: "new" });
  const ticket = await client.beginRequest(marker);
  assert(ticket);
  assert.equal((await client.endRequest(ticket, report())).durable, true);
  const endings = calls.filter(call => call.operation.endsWith("end"));
  assert.equal(endings.length, 2);
  assert.deepEqual(endings[0], endings[1]);
  for (const call of calls) {
    assert(Object.isFrozen(call));
    assert.equal(call.generation, 7);
    assert.equal(call.external_session_id, "session-a");
    assert.equal(call.observer_witness, "a".repeat(64));
    assert(!Object.hasOwn(call, "launch_token"));
    assert(!Object.hasOwn(call, "run_id"));
  }
  const count = calls.length;
  client.stop();
  assert.equal((await client.endRequest(ticket, report())).durable, true);
  assert.equal(calls.length, count, "known ACK replay must remain local after stop");
});

/** Caller-owned currentness cannot admit a successor through old selectors;
 * queued work is inert after stop, and capacity never grows beyond sixteen. */
test("curated accounting bounds queue and never revives a stopped owner", async () => {
  let live = true, release, entered;
  const calls = [];
  const started = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const client = createCuratedRequestAccountingClient({ observer: observer(), isCurrent: () => live,
    send: async capsule => { calls.push(capsule); entered(); return held; } });
  const first = client.beginRequest(marker);
  await started;
  const queued = Array.from({ length: 15 }, () => client.beginRequest(marker));
  assert.equal(await client.beginRequest(marker), undefined);
  live = false;
  release(JSON.stringify(ready()));
  assert.equal(await first, undefined);
  live = true;
  assert((await Promise.all(queued)).every(value => value === undefined));
  assert.equal(await client.beginRequest(marker), undefined);
  assert.equal(calls.length, 1);
});

/** Stop during an actual sent report cannot imply loss or acknowledge an old
 * source afterward. It never sends again even if caller currentness flips back. */
test("curated accounting withholds late ACK without claiming expense was undone", async () => {
  let release, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const held = new Promise(resolve => { release = resolve; });
  const calls = [];
  const client = createCuratedRequestAccountingClient({ observer: observer(), isCurrent: () => true,
    send: async capsule => {
      calls.push(capsule);
      if (capsule.operation.endsWith("begin")) return JSON.stringify(ready());
      entered(); return held;
    } });
  const ticket = await client.beginRequest(marker);
  const pending = client.endRequest(ticket, report());
  await started;
  assert.equal(client.abandonRequest(ticket).reason, "request-report-in-flight");
  client.stop();
  release(JSON.stringify(ack()));
  assert.equal((await pending).durable, false);
  assert.equal(client.abandonRequest(ticket).reason, "request-delivery-unknown-abandoned");
  assert.equal(calls.filter(call => call.operation.endsWith("end")).length, 1);
});

/** Fixed helper success is not a durable receipt. Duplicate/unknown/private
 * fields, unsafe numeric evidence and malformed JSON must remain unavailable. */
test("curated accounting rejects untyped or private helper output", async () => {
  for (const output of ["{}", "not-json", "null", JSON.stringify({ ...ack(), durable: false }),
    JSON.stringify({ ...ack(), launch_token: "PRIVATE" }), JSON.stringify({ ...ack(), revision: 9007199254740992 }),
    JSON.stringify({ ...ack(), coverage: { ...ack().coverage, prompt: "PRIVATE" } }),
    JSON.stringify(ack()).replace('"durable":true', '"durable":false,"durable":true'),
    JSON.stringify(ack()).replace('"source":', '"source":"fake","\\u0073ource":')]) {
    const client = createCuratedRequestAccountingClient({ observer: observer(), isCurrent: () => true,
      send: async capsule => capsule.operation.endsWith("begin") ? JSON.stringify(ready()) : output });
    const ticket = await client.beginRequest(marker);
    assert.equal((await client.endRequest(ticket, report())).durable, false);
  }
});

/** Malformed observer metadata and missing owner proof never launch a helper;
 * field getters are captured once, and private handles cannot be retained. */
test("curated accounting validates observer snapshots and captures selectors once", async () => {
  for (const source of [undefined, {}, { ...observer(), launch_token: "PRIVATE" }, { ...observer(), generation: 0 },
    { ...observer(), generation: 9007199254740992 }, { ...observer(), observer_witness: "bad" }]) {
    const client = createCuratedRequestAccountingClient({ observer: source, isCurrent: () => true,
      send: async () => { assert.fail("invalid observer dispatched"); } });
    assert.equal(await client.beginRequest(marker), undefined);
  }
  let reads = 0;
  const source = observer();
  Object.defineProperty(source, "generation", { enumerable: true, get() { return ++reads === 1 ? 7 : 99; } });
  const client = createCuratedRequestAccountingClient({ observer: source, isCurrent: () => true,
    send: async capsule => { assert.equal(capsule.generation, 7); return JSON.stringify(ready()); } });
  assert(await client.beginRequest(marker));
  assert.equal(reads, 1);
  const unavailable = createCuratedRequestAccountingClient({ observer: observer(),
    send: async () => { assert.fail("missing owner proof dispatched"); } });
  assert.equal(await unavailable.beginRequest(marker), undefined);
});

/** Pure siblings and literal SDK transport must work in a restricted realm.
 * Only fixed argv/options cross the captured runner; stopped original ownership
 * prevents any new helper while known committed ACK replay stays local. */
test("curated SDK runner works without Node globals and uses fixed argv only", async () => {
  const path = name => new URL(`../crates/mezzanine/src/integrations/bootstrap/${name}`, import.meta.url);
  const core = readFileSync(path("request_accounting.mjs"), "utf8")
    .replace("export function createRequestAccountingClient", "function createRequestAccountingClient");
  const adapter = readFileSync(path("curated_request_accounting.mjs"), "utf8")
    .replace("import { createRequestAccountingClient } from './request_accounting.mjs';", "")
    .replace("export function createCuratedRequestAccountingClient", "function createCuratedRequestAccountingClient");
  const context = { createRequestAccountingClient: runInNewContext(`${core}\ncreateRequestAccountingClient;`, {}) };
  const create = runInNewContext(`${adapter}\nif (typeof Buffer !== 'undefined' || typeof TextEncoder !== 'undefined') throw new Error('fixture globals'); createCuratedRequestAccountingClient;`, context);
  const body = readFileSync(path("curated_request_runner.mjs"), "utf8")
    .replace("__MEZ_HELPER__", JSON.stringify("/owned/mez"));
  const calls = [], lifetime = { receipt: Object.freeze(observer()) };
  const sdk = { process: { run: async (argv, options) => {
    calls.push({ argv: Array.from(argv), options });
    const capsule = JSON.parse(argv[2]);
    return { exitCode: 0, stdout: JSON.stringify(capsule.operation.endsWith("begin") ? ready() : ack()) };
  } } };
  const client = runInNewContext(`${body}\nrequestAccounting;`, {
    $: sdk, observerLifetime: lifetime, createCuratedRequestAccountingClient: create,
  });
  const ticket = await client.beginRequest(marker);
  assert.equal((await client.endRequest(ticket, report())).durable, true);
  assert.equal(calls.length, 3);
  for (const call of calls) {
    assert.deepEqual(call.argv.slice(0, 2), ["/owned/mez", "harness-source"]);
    assert.equal(call.options.timeoutMs, 3000);
    assert.equal(Object.keys(call.options).length, 1);
  }
  lifetime.receipt = undefined;
  assert.equal((await client.endRequest(ticket, report())).durable, true);
  assert.equal(await client.beginRequest("e".repeat(64)), undefined);
  lifetime.receipt = Object.freeze(observer());
  assert.equal(await client.beginRequest("e".repeat(64)), undefined);
  assert.equal(calls.length, 3);
});

/** Malformed readiness never issues a ticket. The runner's neutral successful
 * exit and captured stdout are insufficient without strict typed native proof. */
test("curated request readiness and SDK failures remain neutral and bounded", async () => {
  for (const output of ["{}", "x".repeat(4097), "[".repeat(17), JSON.stringify({ ...ready(), controls: ["input"] }),
    JSON.stringify({ ...ready(), request_id: "e".repeat(64) }), JSON.stringify({ ...ready(), admitted_at: 9007199254740992 }),
    JSON.stringify({ ...ready(), owner: "PRIVATE" }), JSON.stringify(ready()).replace('"accepted":true', '"accepted":false,"accepted":true')]) {
    let calls = 0;
    const client = createCuratedRequestAccountingClient({ observer: observer(), isCurrent: () => true,
      send: async () => { calls++; return output; } });
    assert.equal(await client.beginRequest(marker), undefined);
    assert.equal(calls, 1);
  }
  const client = createCuratedRequestAccountingClient({ observer: observer(), isCurrent: () => { throw new Error("PRIVATE"); },
    send: async () => { assert.fail("unavailable owner dispatched"); } });
  assert.equal(await client.beginRequest(marker), undefined);
});
