/** Offline shared-client fixtures with genuine Unix sockets and native peer
 * verification. No vendors, providers, credentials or user config are touched. */
import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:net";
import { mkdtempSync, chmodSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { resolve, isAbsolute } from "node:path";
import { fileURLToPath } from "node:url";
import { createPersistentTelemetryClient, discoverPersistentRoute } from "../crates/mezzanine/src/integrations/bootstrap/persistent_client.mjs";

const helper = process.env.MEZ_TEST_HELPER_BINARY ?? resolve(fileURLToPath(new URL("../target/debug/mez", import.meta.url)));
assert(isAbsolute(helper) && existsSync(helper), "offline shared-client fixture requires an existing absolute built mez helper (MEZ_TEST_HELPER_BINARY)");

/** Owns one short private socket tree, bounded request captures and all peers. */
async function fixture(respond) {
  const directory = mkdtempSync("/tmp/mez-client-");
  chmodSync(directory, 0o700);
  const path = `${directory}/control.sock`;
  const sockets = new Set();
  const requests = [];
  const server = createServer(socket => {
    sockets.add(socket);
    socket.on("error", () => {});
    socket.on("close", () => sockets.delete(socket));
    let buffer = Buffer.alloc(0);
    socket.on("data", bytes => {
      buffer = Buffer.concat([buffer, bytes]);
      assert(buffer.length < 32768);
      const end = buffer.indexOf("\r\n\r\n");
      if (end < 0) return;
      const header = buffer.subarray(0, end).toString().split("\r\n");
      assert.equal(header[1], "Content-Type: application/vnd.mezzanine.control+json; version=1");
      const length = Number(/^Content-Length: ([0-9]+)$/.exec(header[0])[1]);
      if (buffer.length < end + 4 + length) return;
      const request = JSON.parse(buffer.subarray(end + 4, end + 4 + length).toString());
      buffer = buffer.subarray(end + 4 + length);
      requests.push(request);
      assert(requests.length <= 128);
      const value = respond(request, socket);
      if (value !== undefined) {
        const body = JSON.stringify({ jsonrpc: "2.0", id: request.id, result: value });
        socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n${body}`);
      }
    });
  });
  await new Promise(resolve => server.listen(path, resolve));
  chmodSync(path, 0o600);
  const options = { harness: "pi", session: "session-a", instance: "instance-a",
    version: "fixture", displayName: "fixture", peerHelper: helper,
    env: { MEZ: `${path}\x1fsession=fixture\x1fwindow=@1\x1fpane=%1\x1fprotocol=mez-control/1`, MEZ_PANE: "%1" } };
  return { options, requests, sockets,
    async close() {
      for (const socket of sockets) socket.destroy();
      await new Promise(resolve => server.close(resolve));
      rmSync(directory, { recursive: true, force: true });
    },
  };
}

/** Inert normalized registration reply. Fake tokens are never daemon authority. */
function enrollment(request, generation = 1, epoch = 1) {
  return { protocol: "external-agent/1", registered: true, controls: [], generation,
    run_id: 1, observer_epoch: epoch, observer_instance: request.params.observer_instance,
    external_session_id: request.params.external_session_id, launch_token: "a".repeat(43),
    observer_witness: (generation === 7 ? "b" : generation === 2 ? "d" : "c").repeat(64),
    usage: "unavailable-source-continuity" };
}

test("lifecycle pressure refuses accounting explicitly, then retries its same frozen report", async () => {
  let held, entered;
  const started = new Promise(resolve => { entered = resolve; });
  const ready = request => ({ protocol: "external-request/1", accepted: true, request_id: request.params.request_id,
    admitted_at: 100, source_witness: "c".repeat(64), origin: "unattributed", controls: [] });
  const f = await fixture((request, socket) => {
    if (request.method === "agent/external/enroll") return enrollment(request);
    if (request.method === "agent/external/usage-begin") return ready(request);
    if (request.method === "agent/external/usage-end") return { accepted: true, durable: true, applied: true, revision: 1, coverage: { source: "external-reported", reasoning_known: false } };
    if (request.params.sequence === 1) { held = { request, socket }; entered(); return; }
    return { changed: true, sequence: request.params.sequence };
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    const ticket = await client.beginRequest("a".repeat(64));
    assert(ticket, "native helper preflight/readiness must succeed before waiting for a presentation RPC");
    const queued = [client.presentation("running")];
    await started;
    for (let index = 0; index < 15; index++) queued.push(client.presentation("running"));
    const report = { event_id: "b".repeat(64), observed_at: 101, provider: "fixture", model: "model", counters: { input_tokens: 10, output_tokens: 3 } };
    assert.equal((await client.endRequest(ticket, report)).durable, false);
    assert.equal(f.requests.filter(request => request.method === "agent/external/usage-end").length, 0);
    const body = JSON.stringify({ jsonrpc: "2.0", id: held.request.id, result: { changed: true, sequence: 1 } });
    held.socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n${body}`);
    await Promise.all(queued);
    assert.equal((await client.endRequest(ticket, report)).durable, true);
    const sent = f.requests.filter(request => request.method === "agent/external/usage-end");
    assert.equal(sent.length, 1); assert.equal(sent[0].params.counters.input_tokens, 10);
  } finally { client.detach(); await f.close(); }
});

test("loading and invalid discovery remain inert; no native controls or usage surface", async () => {
  for (const env of [{}, { MEZ: "/tmp/socket", MEZ_PANE: "%1" },
    { MEZ: "relative\x1fsession=a\x1fwindow=b\x1fpane=%1\x1fprotocol=mez-control/1", MEZ_PANE: "%1" }]) {
    assert.equal(discoverPersistentRoute(env), undefined);
    const client = createPersistentTelemetryClient({ env });
    assert.equal(await client.start(), false);
    assert.equal((await client.presentation({ prompt: "private" })).delivered, false);
    assert.equal(client.usage, undefined);
    assert.equal(client.initialize, undefined);
    client.detach();
  }
});

test("native peer-verified producer sends only fixed lifecycle metadata", async () => {
  const f = await fixture(request => request.method === "agent/external/enroll" ? enrollment(request)
    : { changed: true, sequence: request.params.sequence, retired: true });
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(f.sockets.size, 0, "factory performed I/O");
    assert.equal(await client.start(), true, `phase=${client.status().phase}; requests=${f.requests.length}`);
    assert.equal(await client.start(), true);
    assert.equal((await client.presentation("running")).delivered, true);
    assert.equal((await client.presentation("complete")).delivered, true);
    assert.equal(client.status().phase, "enrolled");
    assert.equal(client.status().epoch, 1);
    assert(!JSON.stringify(client.status()).includes("launch_token"));
    assert.deepEqual(f.requests.map(request => request.method), ["agent/external/enroll", "agent/external/presentation", "agent/external/presentation"]);
    assert.deepEqual(f.requests.slice(1).map(request => request.params.sequence), [1, 2]);
    assert.deepEqual(Object.keys(f.requests[0].params).sort(), ["display_name", "external_session_id", "harness", "observer_instance", "observer_kind", "pane_id", "version"].sort());
    assert.equal((await client.end()).delivered, true);
  } finally { client.detach(); await f.close(); }
});

test("shared persistent client supports only six canonical labels without adapter attestation", async () => {
  for (const harness of ["claude", "codex", "copilot", "opencode", "cursor", "pi"]) {
    const f = await fixture(request => request.method === "agent/external/enroll" ? enrollment(request)
      : { changed: true, sequence: request.params.sequence, retired: true });
    const client = createPersistentTelemetryClient({ ...f.options, harness });
    try {
      assert.equal(f.sockets.size, 0, "factory performed I/O");
      assert.equal(await client.start(), true, `persistent label ${harness} rejected`);
      assert.equal((await client.presentation("running")).delivered, true);
      assert.equal(f.requests[0].params.harness, harness);
      assert.equal(f.requests[0].params.observer_kind, "persistent");
      assert.equal(client.status().usage, "unavailable-source-continuity");
      assert.equal(client.usage, undefined);
      assert.equal(client.initialize, undefined);
      assert.equal((await client.end()).delivered, true);
    } finally { client.detach(); await f.close(); }
  }
  for (const harness of ["gemini", "unknown", "Claude", "codex-hook"]) {
    const f = await fixture(request => enrollment(request));
    const client = createPersistentTelemetryClient({ ...f.options, harness });
    try {
      assert.equal(await client.start(), false);
      assert.equal(f.sockets.size, 0);
      assert.equal(f.requests.length, 0);
    } finally { client.detach(); await f.close(); }
  }
});

test("successor sends exact predecessor and has its own sequence/private state", async () => {
  const f = await fixture(request => request.method === "agent/external/enroll"
    ? enrollment(request, request.params.observer_instance === "instance-a" ? 1 : 2,
      request.params.observer_instance === "instance-a" ? 1 : 2) : { changed: true, sequence: request.params.sequence });
  const old = createPersistentTelemetryClient(f.options);
  let successor;
  try {
    assert.equal(await old.start(), true);
    assert.equal((await old.presentation("running")).delivered, true);
    successor = old.successor("instance-b");
    assert.equal(await successor.start(), true);
    assert.equal((await successor.presentation("ready")).delivered, true);
    const replacement = f.requests.find(request => request.params.observer_instance === "instance-b");
    assert.equal(replacement.params.predecessor_generation, 1);
    assert.equal(successor.status().epoch, 2);
    assert.equal(f.requests.at(-1).params.sequence, 1);
    old.detach();
    assert.equal(successor.status().phase, "enrolled");
  } finally { old.detach(); successor?.detach(); await f.close(); }
});

test("helper snapshots are immutable original-epoch metadata with one generic sequence owner", async () => {
  const f = await fixture(request => request.method === "agent/external/enroll"
    ? enrollment(request, request.params.observer_instance === "instance-a" ? 7 : 9,
      request.params.observer_instance === "instance-a" ? 1 : 2)
    : { changed: true, sequence: request.params.sequence, accepted: true, retired: false });
  const client = createPersistentTelemetryClient(f.options);
  let successor;
  try {
    assert.equal(await client.captureHelperObservation("running"), undefined);
    assert.equal(f.sockets.size, 0);
    assert.equal(f.requests.length, 0);
    assert.equal(await client.start(), true);
    const snapshot = await client.captureHelperObservation("running");
    assert.deepEqual(snapshot, { operation: "helper-observe", harness: "pi", generation: 7,
      observer_witness: "b".repeat(64),
      external_session_id: "session-a", data: { sequence: 1, state: "running" } });
    assert(Object.isFrozen(snapshot) && Object.isFrozen(snapshot.data));
    assert.throws(() => { snapshot.generation = 9; }, TypeError);
    assert.throws(() => { snapshot.data.state = "failed"; }, TypeError);
    assert(!JSON.stringify(snapshot).includes("launch_token"));
    assert.equal(f.requests.length, 1, "capture performed a transport operation");
    assert.equal((await client.presentation("complete")).delivered, true);
    assert.equal(f.requests.at(-1).params.sequence, 2);
    assert.equal(await client.captureHelperObservation({ prompt: "PRIVATE" }), undefined);
    successor = client.successor("instance-b");
    assert.equal(await successor.start(), true);
    client.detach();
    assert.equal(await client.captureHelperObservation("failed"), undefined);
    const fresh = await successor.captureHelperObservation("ready");
    assert.equal(fresh.generation, 9);
    assert.equal(fresh.data.sequence, 1);
    assert.equal(snapshot.generation, 7, "replacement rebound a captured callback");
    successor.disconnect();
    const requests = f.requests.length;
    assert.equal(await successor.captureHelperObservation("running"), undefined);
    assert.equal(f.requests.length, requests, "lost transport auto-enrolled during capture");
  } finally { client.detach(); successor?.detach(); await f.close(); }
  const pi = await fixture(request => request.method === "agent/external/enroll" ? enrollment(request)
    : { accepted: true, sequence: request.params.sequence, retired: false });
  const typed = createPersistentTelemetryClient(pi.options);
  try {
    assert.equal((await typed.piObservation({ type: "session_start", reason: "startup" })).delivered, true);
    assert.equal(await typed.captureHelperObservation("running"), undefined);
    assert.equal(pi.requests.length, 2);
  } finally { typed.detach(); await pi.close(); }
});

test("queued helper capture cannot cross transport loss or implicit reenrollment", async () => {
  const f = await fixture(request => {
    if (request.method !== "agent/external/enroll") return undefined;
    // An ordinary reconnect receives the same full current handle. A changed
    // handle belongs to the separately tested typed recovery contract.
    return enrollment(request, 7);
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(await client.start(), true);
    const pending = client.presentation("running");
    for (let attempt = 0; f.requests.length < 2 && attempt < 100; attempt++) {
      await new Promise(resolve => setTimeout(resolve, 5));
    }
    assert.equal(f.requests.length, 2);
    const queued = client.captureHelperObservation("complete");
    client.disconnect();
    assert.equal((await pending).delivered, false);
    assert.equal(await queued, undefined);
    assert.equal(f.requests.length, 2, "capture implicitly re-enrolled");
    assert.equal(await client.start(), true);
    const fresh = await client.captureHelperObservation("ready");
    assert.equal(fresh.generation, 7);
    assert.equal(fresh.observer_witness, "b".repeat(64));
    assert.equal(fresh.data.sequence, 2, "cancelled capture consumed a sequence");
  } finally { client.detach(); await f.close(); }
});

test("missing or malformed observer-instance witnesses cannot enroll", async () => {
  for (const witness of [undefined, null, "", "B".repeat(64), "b".repeat(63), "b".repeat(65), {}]) {
    const f = await fixture(request => ({ ...enrollment(request), observer_witness: witness }));
    const client = createPersistentTelemetryClient(f.options);
    try {
      assert.equal(await client.start(), false);
      assert.equal(await client.captureHelperObservation("running"), undefined);
      assert.equal(f.requests.length, 1);
    } finally { client.detach(); await f.close(); }
  }
});

test("malformed or unavailable peer/helper remains neutral and does not enroll", async () => {
  const f = await fixture(request => enrollment(request));
  const client = createPersistentTelemetryClient({ ...f.options, peerHelper: "/missing/mez-helper" });
  try {
    assert.equal(await client.start(), false);
    assert.equal((await client.presentation("running")).delivered, false);
    assert.equal(f.requests.length, 0);
  } finally { client.detach(); await f.close(); }
});

test("duplicate keys, mismatched media types and untyped replies cannot enroll", async () => {
  for (const mode of ["duplicate-id", "escaped-id", "duplicate-token", "wrong-type", "untyped"]) {
    const f = await fixture((request, socket) => {
      const result = enrollment(request);
      let body = JSON.stringify({ jsonrpc: "2.0", id: request.id, result });
      if (mode === "duplicate-id") body = body.replace('"result":', `"id":${JSON.stringify(request.id)},"result":`);
      if (mode === "escaped-id") body = body.replace('"result":', `"\\u0069d":${JSON.stringify(request.id)},"result":`);
      if (mode === "duplicate-token") body = body.replace('"launch_token":', '"launch_token":"bad","launch_token":');
      if (mode === "untyped") body = JSON.stringify({ jsonrpc: "2.0", id: request.id, result: {} });
      const type = mode === "wrong-type" ? "application/json" : "application/vnd.mezzanine.control+json; version=1";
      socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: ${type}\r\n\r\n${body}`);
    });
    const client = createPersistentTelemetryClient(f.options);
    try {
      assert.equal(await client.start(), false, mode);
      assert.equal(client.status().phase, "connection-lost");
      assert.equal(f.requests.length, 1);
    } finally { client.detach(); await f.close(); }
  }
});

test("presentation receipt must acknowledge the exact sequence and boolean change", async () => {
  const f = await fixture(request => request.method === "agent/external/enroll" ? enrollment(request) : {});
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(await client.start(), true);
    assert.equal((await client.presentation("running")).delivered, false);
    assert.equal(client.status().phase, "connection-lost");
  } finally { client.detach(); await f.close(); }
});

test("a stopped private helper cannot retain the fake vendor after timeout", async () => {
  for (const mode of ["timeout", "detach"]) {
  const f = await fixture(() => assert.fail("stalled checker sent telemetry"));
  const stalled = `${resolve(f.options.env.MEZ.split("\x1f")[0], "..")}/stalled-helper`;
  writeFileSync(stalled, "#!/bin/sh\nkill -STOP $$\n", { mode: 0o700 });
  let producer;
  try {
    const script = fileURLToPath(new URL("./persistent-client-exit-fixture.mjs", import.meta.url));
    producer = spawn(process.execPath, [script, f.options.env.MEZ.split("\x1f")[0], stalled, mode],
      { stdio: ["ignore", "pipe", "pipe"] });
    let output = "";
    producer.stdout.on("data", bytes => { output += bytes; assert(output.length < 1024); });
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { producer.kill("SIGKILL"); reject(new Error("telemetry retained fake vendor")); }, 4000);
      producer.once("error", error => { clearTimeout(timer); reject(error); });
      producer.once("close", code => { clearTimeout(timer); code === 0 ? resolve() : reject(new Error("exit fixture failed")); });
    });
    const observed = JSON.parse(output);
    assert.equal(observed.helperSpawned, true);
    assert.equal(observed.result, false);
    assert.equal(observed.phase, "connection-lost");
    assert.equal(observed.childReferenced, false, "timed-out helper retained process reference");
    assert.equal(observed.stdoutReferenced, false, "timed-out helper retained pipe reference");
    assert.equal(f.requests.length, 0);
  } finally { if (producer?.exitCode === null) producer.kill("SIGKILL"); await f.close(); }
  }
});

test("unsafe/malformed replies and bounded callback pressure lose telemetry only", async () => {
  const f = await fixture((request, socket) => {
    if (request.method === "agent/external/enroll") return enrollment(request);
    socket.write("Content-Length: 9000\r\n\r\n");
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(await client.start(), true);
    const results = await Promise.all(Array.from({ length: 40 }, () => client.presentation("running")));
    assert(results.some(result => !result.delivered));
    assert.equal(client.status().phase, "connection-lost");
    assert(f.requests.length <= 32, "unbounded producer retry pressure");
  } finally { client.detach(); await f.close(); }
});

test("Pi fact transport captures getter selectors once and isolates generic sources", async () => {
  const f = await fixture(request => request.method === "agent/external/enroll" ? enrollment(request)
    : { accepted: true, sequence: request.params.sequence, retired: false });
  const client = createPersistentTelemetryClient(f.options);
  try {
    let reads = 0;
    const event = { type: "session_start", prompt: "PRIVATE",
      get reason() { return ++reads === 1 ? "startup" : "PRIVATE"; } };
    assert.equal((await client.piObservation(event)).delivered, true);
    assert.equal(reads, 1);
    assert.deepEqual(f.requests.at(-1).params.event, { type: "session_start", reason: "startup" });
    assert(!JSON.stringify(f.requests).includes("PRIVATE"));
    assert.equal((await client.presentation("running")).delivered, false);
    assert.equal((await client.piObservation({ type: "agent_end" })).delivered, false);
    let gapReads = 0;
    assert.equal((await client.piObservation({ type: "usage_gap", prompt: "PRIVATE", amount: 999,
      get reason() { return ++gapReads === 1 ? "durable-acknowledgment" : "PRIVATE"; } })).delivered, true);
    assert.equal(gapReads, 1);
    assert.deepEqual(f.requests.at(-1).params.event, { type: "usage_gap", reason: "durable-acknowledgment" });
    assert(!JSON.stringify(f.requests).includes("PRIVATE"));
    const count = f.requests.length;
    assert.equal((await client.piObservation({ type: "usage_gap", reason: "PRIVATE" })).delivered, false);
    assert.equal(f.requests.length, count);
  } finally { client.detach(); await f.close(); }
});

test("connected-only lifecycle delivery never auto-enrolls after a socket loss", async () => {
  const f = await fixture(request => request.method === "agent/external/enroll" ? enrollment(request)
    : { changed: true, sequence: request.params.sequence });
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(await client.start(), true);
    client.disconnect();
    const count = f.requests.length;
    assert.equal((await client.presentation("running", true)).delivered, false);
    assert.equal(f.requests.length, count, "connected-only delivery opened another enrollment");
  } finally { client.detach(); await f.close(); }
});

/** Native-shaped resume receipts reset only lifecycle ordinal and preserve the
 * original public selector/run. This mock transport proves client parsing,
 * not server/native recovery authority or durable accounting continuity. */
test("typed same-live recovery sends captured selector and resets ordinal once", async () => {
  let admissions = 0;
  const f = await fixture(request => {
    if (request.method === "agent/external/enroll") {
      admissions++;
      if (admissions === 1) return enrollment(request, 7, 1);
      return { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    }
    return { changed: true, sequence: request.params.sequence };
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(await client.start(), true);
    assert.equal((await client.presentation("running")).delivered, true);
    assert.equal((await client.presentation("ready")).delivered, true);
    client.disconnect();
    assert.equal(await client.start(), true);
    const resume = f.requests.filter(request => request.method.endsWith("enroll")).at(-1);
    assert.deepEqual(resume.params.source_resume, { observer_witness: "b".repeat(64), generation: 7 });
    assert.equal(resume.params.predecessor_generation, undefined);
    assert.equal((await client.presentation("running")).delivered, true);
    assert.equal(f.requests.at(-1).params.sequence, 1);
    assert.equal(client.status().run, 1); assert.equal(client.status().epoch, 2);
  } finally { client.detach(); await f.close(); }
});

/** A lost recovery reply retains exact S0/P. Accepting H1 then reconnecting uses
 * S1, while a repeated already-accepted H1 receipt preserves advanced ordinals. */
test("recovery lost reply keeps S0 and repeated accepted handle never resets twice", async () => {
  let admissions = 0;
  const f = await fixture((request, socket) => {
    if (request.method.endsWith("enroll")) {
      if (++admissions === 1) return enrollment(request, 7, 1);
      if (admissions === 2) { socket.destroy(); return; }
      return { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    }
    return { changed: true, sequence: request.params.sequence };
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    assert.equal(await client.start(), true); await client.presentation("running"); client.disconnect();
    assert.equal(await client.start(), false); assert.equal(await client.start(), true);
    const attempts = f.requests.filter(request => request.method.endsWith("enroll"));
    assert.deepEqual(attempts[1].params, attempts[2].params);
    assert.deepEqual(attempts[1].params.source_resume, { observer_witness: "b".repeat(64), generation: 7 });
    await client.presentation("running"); assert.equal(f.requests.at(-1).params.sequence, 1);
    client.disconnect(); assert.equal(await client.start(), true);
    assert.deepEqual(f.requests.filter(request => request.method.endsWith("enroll")).at(-1).params.source_resume, { observer_witness: "e".repeat(64), generation: 8 });
    await client.presentation("ready"); assert.equal(f.requests.at(-1).params.sequence, 2);
  } finally { client.detach(); await f.close(); }
});

/** Successor replay carries only captured public parent selectors and original
 * predecessor. A failed reply cannot refresh that predecessor or copy tickets. */
test("successor resume attempt preserves original predecessor across reply loss", async () => {
  let successorAdmissions = 0;
  const f = await fixture((request, socket) => {
    if (!request.method.endsWith("enroll")) return { changed: true, sequence: request.params.sequence };
    if (request.params.observer_instance === "instance-a") return enrollment(request, 7, 1);
    if (++successorAdmissions === 1) { socket.destroy(); return; }
    return { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
  });
  const parent = createPersistentTelemetryClient(f.options); let successor;
  try {
    assert.equal(await parent.start(), true); assert.equal(parent.successor("instance-a"), undefined);
    successor = parent.successor("instance-b"); assert.equal(await successor.start(), false); assert.equal(await successor.start(), true);
    const sent = f.requests.filter(request => request.params.observer_instance === "instance-b");
    assert.deepEqual(sent[0].params, sent[1].params); assert.equal(sent[1].params.predecessor_generation, 7);
    assert.deepEqual(sent[1].params.source_resume, { observer_witness: "b".repeat(64), generation: 7 });
    assert(!Object.hasOwn(sent[1].params, "launch_token"));
    successor.disconnect(); assert.equal(await successor.start(), true);
    assert.equal(f.requests.at(-1).params.predecessor_generation, 7);
    assert.deepEqual(f.requests.at(-1).params.source_resume, { observer_witness: "e".repeat(64), generation: 8 });
  } finally { parent.detach(); successor?.detach(); await f.close(); }
});

/** Recovery is opt-in and exact; generic success, changed fields, unsafe epoch
 * arithmetic or an unsolicited/malformed recovery cannot reset projection. */
test("recovery parser rejects unrequested malformed and changed-identity receipts", async () => {
  const bad = [null, [], {}, { kind: "same-live-source", next_sequence: "1" }, { kind: "same-live-source", next_sequence: 2 },
    { kind: "other", next_sequence: 1 }, { kind: "same-live-source", next_sequence: 1, extra: true }];
  for (const recovery of bad) {
    let n = 0;
    const f = await fixture(request => ++n === 1 ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery });
    const client = createPersistentTelemetryClient(f.options);
    try { assert.equal(await client.start(), true); client.disconnect(); assert.equal(await client.start(), false); assert.equal(client.status().epoch, 1); }
    finally { client.detach(); await f.close(); }
  }
  for (const changed of [{ run_id: 2 }, { observer_epoch: 1 }, { observer_epoch: 3 }, { observer_epoch: Number.MAX_SAFE_INTEGER + 1 },
    { launch_token: "a".repeat(43) }, { observer_witness: "b".repeat(64) }, { observer_instance: "newest" }, { external_session_id: "other" }]) {
    let n = 0;
    const f = await fixture(request => ++n === 1 ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 }, ...changed });
    const client = createPersistentTelemetryClient(f.options);
    try { assert.equal(await client.start(), true); client.disconnect(); assert.equal(await client.start(), false); }
    finally { client.detach(); await f.close(); }
  }
  const f = await fixture(request => ({ ...enrollment(request), recovery: { kind: "same-live-source", next_sequence: 1 } }));
  const client = createPersistentTelemetryClient(f.options);
  try { assert.equal(await client.start(), false); } finally { client.detach(); await f.close(); }
});

/** Supplied parent selectors are never repaired from labels/paths or unsafe
 * numbers. Invalid options remain resource-free; strict server proof is separate. */
test("resume option copying rejects unsafe selectors before opening resources", async () => {
  for (const sourceResume of [null, [], {}, { observer_witness: "B".repeat(64), generation: 1 }, { observer_witness: "b".repeat(63), generation: 1 },
    ...[0, -1, 0.5, Number.MAX_SAFE_INTEGER + 1, "1"].map(generation => ({ observer_witness: "b".repeat(64), generation })),
    { observer_witness: "b".repeat(64), generation: 1, launch_token: "PRIVATE" }]) {
    const f = await fixture(request => enrollment(request));
    const client = createPersistentTelemetryClient({ ...f.options, sourceResume, sourceRun: 1, sourceEpoch: 1 });
    try { assert.equal(await client.start(), false); assert.equal(f.requests.length, 0); assert.equal(f.sockets.size, 0); }
    finally { client.detach(); await f.close(); }
  }
});

/** Observer identity recovery never changes durable request source, origin,
 * marker, counters or UTC. The real request client revalidates readiness and
 * emits its exact retained report under the fresh transport credential only. */
test("same-live transport recovery preserves immutable accounting ticket and report", async () => {
  let admissions = 0;
  const f = await fixture(request => {
    if (request.method.endsWith("enroll")) return ++admissions === 1 ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    if (request.method.endsWith("usage-begin")) return { protocol: "external-request/1", accepted: true, request_id: request.params.request_id,
      admitted_at: 100, source_witness: "c".repeat(64), origin: "project", controls: [] };
    return { accepted: true, durable: true, applied: false, revision: 5, coverage: { source: "external-reported", reasoning_known: false } };
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    const ticket = await client.beginRequest("a".repeat(64)); const original = JSON.stringify(ticket);
    client.disconnect(); const report = { event_id: "b".repeat(64), observed_at: 101, provider: "fixture", model: "model", counters: { input_tokens: 10, output_tokens: 3 } };
    const receipt = await client.endRequest(ticket, report); assert.equal(receipt.durable, true); assert.equal(receipt.applied, false);
    assert.equal(JSON.stringify(ticket), original); assert(Object.isFrozen(ticket));
    const sent = f.requests.find(request => request.method.endsWith("usage-end"));
    assert.deepEqual(sent.params.counters, report.counters); assert.equal(sent.params.request_id, ticket.request_id); assert.equal(sent.params.observed_at, 101);
    assert.equal(sent.params.generation, 8); assert.equal(sent.params.launch_token, "d".repeat(43));
    assert.equal(client.status().run, 1); assert.equal(client.status().epoch, 2);
  } finally { client.detach(); await f.close(); }
});

/** Lifecycle recovery cannot overwrite accounting provenance. A restored
 * observer may still be unable to send an old report if its original source
 * witness or origin is missing; no usage-end is sent under a replacement. */
test("accounting source mismatch stays rejected after valid observer recovery", async () => {
  let n = 0;
  const f = await fixture(request => {
    if (request.method.endsWith("enroll")) return ++n === 1 ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    return { protocol: "external-request/1", accepted: true, request_id: request.params.request_id, admitted_at: 100,
      source_witness: (n === 1 ? "c" : "f").repeat(64), origin: "unattributed", controls: [] };
  });
  const client = createPersistentTelemetryClient(f.options);
  try {
    const ticket = await client.beginRequest("a".repeat(64)); client.disconnect();
    const reply = await client.endRequest(ticket, { event_id: "b".repeat(64), observed_at: 101, provider: "fixture", model: "model", counters: { input_tokens: 1, output_tokens: 1 } });
    assert.equal(reply.reason, "request-source-changed"); assert.equal(client.status().epoch, 2);
    assert.equal(f.requests.filter(request => request.method.endsWith("usage-end")).length, 0);
    assert.equal(ticket.source_witness, "c".repeat(64));
  } finally { client.detach(); await f.close(); }
});

/** Successor metadata carries known public parent expectations even on normal
 * rotation. A wrong run/epoch or same witness cannot become a newest owner. */
test("normal successor rejects changed public parent expectations and detached parent", async () => {
  for (const bad of [{ run_id: 2 }, { observer_epoch: 1 }, { observer_epoch: 3 }, { observer_witness: "b".repeat(64) }]) {
    const f = await fixture(request => request.params.observer_instance === "instance-a" ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), observer_witness: "e".repeat(64), ...bad });
    const parent = createPersistentTelemetryClient(f.options); let child;
    try { assert.equal(await parent.start(), true); child = parent.successor("instance-b"); assert.equal(await child.start(), false);
      parent.detach(); assert.equal(parent.successor("instance-c"), undefined); }
    finally { parent.detach(); child?.detach(); await f.close(); }
  }
});

/** A changed same-instance receipt without typed recovery never resets by
 * inference. Legacy extra normal fields remain tolerated for compatibility. */
test("untyped changed handle rejected while extra normal receipt fields remain compatible", async () => {
  let n = 0;
  const f = await fixture(request => ++n === 1 ? { ...enrollment(request, 7, 1), future_diagnostic: "bounded" }
    : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64) });
  const client = createPersistentTelemetryClient(f.options);
  try { assert.equal(await client.start(), true); client.disconnect(); assert.equal(await client.start(), false); assert.equal(client.status().epoch, 1); }
  finally { client.detach(); await f.close(); }
});

/** An unknown accepted resume cannot be reinterpreted as bare enrollment when
 * its ephemeral binding is lost again. Identical failed attempts keep selectors. */
test("repeated reset named error does not drop resume selectors or original predecessor", async () => {
  let n = 0;
  const f = await fixture((request, socket) => {
    if (++n === 1) return enrollment(request, 7, 1);
    if (n === 2) { socket.destroy(); return; }
    const body = JSON.stringify({ jsonrpc: "2.0", id: request.id, error: { code: -32009, message: "source resume retry unavailable" } });
    socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n${body}`);
  });
  const client = createPersistentTelemetryClient(f.options);
  try { assert.equal(await client.start(), true); client.disconnect(); assert.equal(await client.start(), false);
    assert.equal(await client.start(), false); assert.equal(await client.start(), false);
    assert.deepEqual(f.requests.slice(1).map(request => request.params), Array(3).fill(f.requests[1].params));
    assert(Object.hasOwn(f.requests.at(-1).params, "source_resume")); assert.equal(client.status().epoch, 1); }
  finally { client.detach(); await f.close(); }
});

/** A queued old helper capture is fenced by both original handle and transport
 * attempt. Typed recovery may create a new basis but cannot publish old work. */
test("queued helper capture stays stale across typed changed-handle recovery", async () => {
  let n = 0; let entered; const sent = new Promise(resolve => { entered = resolve; });
  const f = await fixture(request => {
    if (request.method.endsWith("enroll")) return ++n === 1 ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    entered(); return undefined;
  });
  const client = createPersistentTelemetryClient(f.options);
  try { assert.equal(await client.start(), true); const pending = client.presentation("running"); await sent;
    const capture = client.captureHelperObservation("complete"); client.disconnect();
    assert.equal((await pending).delivered, false); assert.equal(await capture, undefined);
    assert.equal(await client.start(), true); const fresh = await client.captureHelperObservation("ready");
    assert.equal(fresh.generation, 8); assert.equal(fresh.observer_witness, "e".repeat(64)); assert.equal(fresh.data.sequence, 1);
  } finally { client.detach(); await f.close(); }
});

/** An enrollment response cannot resurrect a detached client. Reply framing is
 * real but native/source admission is mocked; the resulting state stays closed. */
test("late resumed admission reply cannot publish after detach", async () => {
  let n = 0; let held; let entered; const sent = new Promise(resolve => { entered = resolve; });
  const f = await fixture((request, socket) => { if (++n === 1) return enrollment(request, 7, 1);
    held = { request, socket }; entered(); return undefined; });
  const client = createPersistentTelemetryClient(f.options);
  try { assert.equal(await client.start(), true); client.disconnect(); const pending = client.start(); await sent; client.detach();
    const result = { ...enrollment(held.request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    const body = JSON.stringify({ jsonrpc: "2.0", id: held.request.id, result });
    if (!held.socket.destroyed) held.socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n${body}`);
    assert.equal(await pending, false); assert.equal(client.status().epoch, 1); assert.equal(await client.start(), false);
  } finally { client.detach(); await f.close(); }
});

/** Recursively strict framing rejects escaped duplicate recovery aliases before
 * parsing can erase ambiguity; checked epoch overflow cannot become a reset. */
test("duplicate recovery fields and epoch overflow fail closed", async () => {
  for (const overflow of [false, true]) {
    let n = 0;
    const f = await fixture((request, socket) => {
      if (++n === 1) return enrollment(request, 7, overflow ? Number.MAX_SAFE_INTEGER : 1);
      const result = { ...enrollment(request, 8, overflow ? Number.MAX_SAFE_INTEGER : 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64) };
      const encoded = JSON.stringify(result).slice(0, -1) + ',"recovery":{"kind":"same-live-source","next_sequence":1,"next_\\u0073equence":1}}';
      const body = overflow ? JSON.stringify({ jsonrpc: "2.0", id: request.id, result: { ...result, recovery: { kind: "same-live-source", next_sequence: 1 } } })
        : `{"jsonrpc":"2.0","id":${JSON.stringify(request.id)},"result":${encoded}}`;
      socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n${body}`);
    });
    const client = createPersistentTelemetryClient(f.options);
    try { assert.equal(await client.start(), true); client.disconnect(); assert.equal(await client.start(), false); }
    finally { client.detach(); await f.close(); }
  }
});

/** Fresh projection recovery sends no synthetic callback. The first actual
 * usage-gap fact allocates ordinal1; a lost ACK is visibly failed, not silently
 * replayed/incremented or treated as accounting commitment. */
test("first actual Pi gap after resume uses ordinal1 and lost ACK remains failed", async () => {
  let n = 0;
  const f = await fixture((request, socket) => {
    if (request.method.endsWith("enroll")) return ++n === 1 ? enrollment(request, 7, 1)
      : { ...enrollment(request, 8, 2), launch_token: "d".repeat(43), observer_witness: "e".repeat(64), recovery: { kind: "same-live-source", next_sequence: 1 } };
    if (request.params.event.type === "usage_gap") { socket.destroy(); return; }
    return { accepted: true, sequence: request.params.sequence, retired: false };
  });
  const client = createPersistentTelemetryClient(f.options);
  try { assert.equal((await client.piObservation({ type: "session_start", reason: "startup" })).delivered, true);
    client.disconnect(); assert.equal(await client.start(), true);
    assert.equal(f.requests.filter(request => request.method.endsWith("pi-observation")).length, 1, "recovery fabricated baseline callback");
    assert.equal((await client.piObservation({ type: "usage_gap", reason: "durable-acknowledgment" })).delivered, false);
    assert.equal(f.requests.at(-1).params.sequence, 1); assert.equal(client.status().phase, "connection-lost");
  } finally { client.detach(); await f.close(); }
});
