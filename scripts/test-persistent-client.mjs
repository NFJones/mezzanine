/** Offline shared-client fixtures with genuine Unix sockets and native peer
 * verification. No vendors, providers, credentials or user config are touched. */
import assert from "node:assert/strict";
import test from "node:test";
import { createServer } from "node:net";
import { mkdtempSync, chmodSync, rmSync, writeFileSync } from "node:fs";
import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createPersistentTelemetryClient, discoverPersistentRoute } from "../crates/mezzanine/src/integrations/bootstrap/persistent_client.mjs";

const helper = resolve(fileURLToPath(new URL("../target/debug/mez", import.meta.url)));

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
    usage: "unavailable-source-continuity" };
}

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
