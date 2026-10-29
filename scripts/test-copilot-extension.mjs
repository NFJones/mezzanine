/** Offline fixed-method Unix transport tests. Synthetic replies exercise the
 * installed source's bounds and public-selector parsing; they do not prove
 * native authority, vendor loading, health or accounting. Rust native tests
 * separately supply real writer/creator/root evidence. No provider is used.
 */
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { chmod, mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { attachCopilotObserver, pulseCopilotObserver } from "../crates/mezzanine/src/integrations/bootstrap/copilot_extension.mjs";

const type = "application/vnd.mezzanine.control+json; version=1";
const peerHelper = process.env.MEZ_TEST_PEER_HELPER;
assert(peerHelper?.startsWith("/"), "explicit built MEZ_TEST_PEER_HELPER path required for native peer check");
const receipt = () => ({ jsonrpc: "2.0", id: "copilot-attach", result: {
  protocol: "external-agent/1", registered: true, controls: [], agent_id: "agent-a",
  generation: 1, observer_witness: "a".repeat(64), run_id: 2, observer_epoch: 1,
  observer_instance: "copilot-local-hooks-1", external_session_id: "session-a",
  usage: "unavailable-source-continuity", observer_transport: "unavailable-curated-freshness",
  expires_at_unix_seconds: 123, lease_seconds: 60,
} });
const frame = raw => `Content-Length: ${Buffer.byteLength(raw)}\r\nContent-Type: ${type}\r\n\r\n${raw}`;

/** Serve one synthetic response only after receiving the entire real request.
 * Always close owned sockets/server/tree, including failed assertions.
 */
async function exchange(reply, check = () => {}, fragmented = false, helper = peerHelper, expectedRequests = 1, operation = "attach") {
  const directory = await mkdtemp("/tmp/mez-copilot-transport-");
  const path = join(directory, "control.sock");
  const sockets = new Set();
  let requests = 0;
  let failure;
  const server = createServer(socket => {
    sockets.add(socket);
    socket.on("close", () => sockets.delete(socket));
    socket.on("error", () => {});
    let bytes = Buffer.alloc(0);
    socket.on("data", chunk => {
      bytes = Buffer.concat([bytes, chunk]);
      const end = bytes.indexOf("\r\n\r\n");
      if (end < 0) return;
      const length = Number(/Content-Length: ([0-9]+)/.exec(bytes.toString())[1]);
      if (bytes.length < end + 4 + length) return;
      requests++;
      try {
        assert.equal(bytes.subarray(0, end).toString(), `Content-Length: ${length}\r\nContent-Type: ${type}`);
        assert.equal(bytes.length, end + 4 + length);
        check(JSON.parse(bytes.subarray(end + 4).toString()));
        if (reply === null) return;
        if (fragmented) {
          socket.write(reply.slice(0, 7));
          setImmediate(() => socket.end(reply.slice(7)));
        } else socket.end(reply);
      } catch (error) { failure = error; socket.destroy(); }
    });
  });
  try {
    await new Promise((resolve, reject) => { server.once("error", reject); server.listen(path, resolve); });
    await chmod(path, 0o600);
    const options = { mez: `${path}\x1fPRIVATE-UNUSED-DISCOVERY`, pane: "%1", session: "session-a", peerHelper: helper };
    const original = { external_session_id: "session-a", generation: 1, observer_witness: "a".repeat(64), run_id: 2, observer_epoch: 1, agent_id: "agent-a" };
    const result = operation === "pulse" ? await pulseCopilotObserver(options, original, "b".repeat(64), 1)
      : await attachCopilotObserver(options);
    if (failure) throw failure;
    assert.equal(requests, expectedRequests, "attachment must not retry or write before native peer qualification");
    return result;
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
}

/** Exactly the fixed attachment method and inert selectors cross the wire.
 * Fragmented replies must preserve one immutable original public selector set.
 */
test("Copilot extension attaches with one fixed content-free frame", async () => {
  const result = await exchange(frame(JSON.stringify(receipt())), request => {
    assert.deepEqual(request, { jsonrpc: "2.0", id: "copilot-attach",
      method: "agent/external/copilot-attach", params: { pane_id: "%1", external_session_id: "session-a" } });
    assert(!JSON.stringify(request).includes("PRIVATE"));
  }, true);
  assert.deepEqual(result, { external_session_id: "session-a", generation: 1,
    observer_witness: "a".repeat(64), run_id: 2, observer_epoch: 1, agent_id: "agent-a" });
  assert(Object.isFrozen(result));
});

/** Rejection, changed epochs/sessions, private extras and duplicate aliases
 * cannot turn arbitrary reply data into current observer selectors.
 */
test("Copilot attachment rejects ambiguous or private public receipts", async () => {
  const replies = [];
  for (const [key, value] of [["external_session_id", "other"], ["observer_epoch", 2],
    ["observer_instance", "replacement"], ["generation", 0], ["controls", ["send"]],
    ["lease_seconds", 600], ["launch_token", "PRIVATE"], ["usage", "durable"]]) {
    const reply = receipt(); reply.result[key] = value; replies.push(JSON.stringify(reply));
  }
  replies.push('{"jsonrpc":"2.0","id":"copilot-attach","error":{"message":"PRIVATE"}}');
  replies.push(JSON.stringify(receipt()).replace('"generation":1', '"generation":1,"generat\\u0069on":1'));
  replies.push(JSON.stringify(receipt()).replace('"id":"copilot-attach"', '"id":"copilot-attach","id":"copilot-attach"'));
  for (const reply of replies) assert.equal(await exchange(frame(reply)), undefined);
});

/** Physical framing limits fail closed, including short bodies, oversized
 * lengths, extra frames, missing content type and invalid UTF8 body bytes.
 */
test("Copilot attachment bounds malformed and oversized framed replies", async () => {
  const raw = JSON.stringify(receipt());
  for (const reply of [
    "x".repeat(300), `Content-Length: 8193\r\nContent-Type: ${type}\r\n\r\n`,
    `Content-Length: 2\r\n\r\n{}`, frame(raw).slice(0, -3), frame(raw) + frame(raw),
    Buffer.concat([Buffer.from(`Content-Length: 1\r\nContent-Type: ${type}\r\n\r\n`), Buffer.from([0xff])]),
  ]) assert.equal(await exchange(reply), undefined);
});

/** Malformed routing/session hints are inert before connection, and missing
 * sockets do not trigger alternate route discovery, helper launch or retries.
 */
test("Copilot attachment refuses invalid hints without fallback", async () => {
  for (const fields of [undefined, {}, { mez: "relative", pane: "%1", session: "session-a" },
    { mez: "/tmp/absent", pane: "%1", session: "x".repeat(129) },
    { mez: "/tmp/absent", pane: "%1\n", session: "session-a" },
    { mez: "/tmp/bad\nroute", pane: "%1", session: "session-a" },
    { mez: "/tmp/mez-copilot-deliberately-absent.sock", pane: "%1", session: "session-a" },
  ]) assert.equal(await attachCopilotObserver(fields), undefined);
});

/** A connected peer that never replies cannot retain the extension's startup
 * work indefinitely. The timeout closes its socket without changing routes or
 * resending, and the fixture tears down only its own server and tree.
 */
test("Copilot attachment times out a stalled peer without retry", { timeout: 10000 }, async () => {
  const start = performance.now();
  assert.equal(await exchange(null), undefined);
  assert(performance.now() - start >= 2500);
});

/** A missing checker or a negative native-check acknowledgment must prevent
 * every metadata byte from being sent, rather than fall back to path ownership
 * or trust an unsolicited response. The fake negative checker grants nothing.
 */
test("Copilot attachment writes nothing without a positive peer check", async () => {
  assert.equal(await exchange(frame(JSON.stringify(receipt())), () => assert.fail("metadata sent"), false,
    "/tmp/mez-copilot-deliberately-absent-helper", 0), undefined);
  const directory = await mkdtemp("/tmp/mez-copilot-negative-peer-");
  const helper = join(directory, "checker");
  try {
    await writeFile(helper, "#!/bin/sh\nprintf '%s\\n' '{\"protocol\":\"external-peer/1\",\"verified\":false}'\n", { mode: 0o700 });
    assert.equal(await exchange(frame(JSON.stringify(receipt())), () => assert.fail("metadata sent"), false,
      helper, 0), undefined);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

/** Pulses carry only original public selectors, a nonce and counter. Typed
 * refusal stops the local loop, while transient errors cannot forge an ACK.
 */
test("Copilot pulse transport uses fixed original selectors and typed ACKs", async () => {
  const envelope = result => JSON.stringify({ jsonrpc: "2.0", id: "copilot-pulse", result });
  assert.deepEqual(await exchange(frame(envelope({ observed: true, sequence: 1, changed: false })), request => {
    assert.deepEqual(request, { jsonrpc: "2.0", id: "copilot-pulse", method: "agent/external/copilot-pulse",
      params: { pane_id: "%1", external_session_id: "session-a", generation: 1,
        observer_witness: "a".repeat(64), observer_id: "b".repeat(64), sequence: 1 } });
  }, false, peerHelper, 1, "pulse"), { observed: true, sequence: 1, changed: false });
  for (const result of [{ observed: true, sequence: 2, changed: true }, { observed: true, sequence: 1, changed: true, title: "PRIVATE" }]) {
    assert.equal(await exchange(frame(envelope(result)), undefined, false, peerHelper, 1, "pulse"), undefined);
  }
  const error = code => JSON.stringify({ jsonrpc: "2.0", id: "copilot-pulse", error: { code, message: "PRIVATE" } });
  assert.deepEqual(await exchange(frame(error(-32002)), undefined, false, peerHelper, 1, "pulse"), { observed: false, terminal: true });
  assert.equal(await exchange(frame(error(-32000)), undefined, false, peerHelper, 1, "pulse"), undefined);
});
