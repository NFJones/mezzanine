/** Original-epoch attachment and concrete idle proof for ordinary Copilot.
 * This module performs no I/O on import. It sends one fixed bounded Unix frame
 * from the extension itself: forwarding through a helper would change the native parent
 * relationship. Routing/session values only select a candidate; the daemon
 * independently proves the socket writer, original creator and pane root.
 * No startup boundary, usage, content, vendor decision or provider request
 * is synthesized. Pulses retain an independent original native writer counter. Failure remains unavailable and is never printed.
 */
import { createConnection } from "node:net";
import { spawn } from "node:child_process";
import { lstatSync } from "node:fs";
import { dirname, isAbsolute } from "node:path";

const identifier = value => typeof value === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(value);
const positive = value => Number.isSafeInteger(value) && value > 0;
const digest = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const contentType = "application/vnd.mezzanine.control+json; version=1";
const exact = (value, fields) => value && typeof value === "object" && !Array.isArray(value)
  && Object.keys(value).length === fields.length && Object.keys(value).every(key => fields.includes(key));

/** Filesystem checks narrow a route/helper before native peer verification.
 * They never substitute for the connected socket's kernel peer UID.
 */
function trustedPaths(route, helper) {
  const uid = process.geteuid?.();
  if (!Number.isInteger(uid) || typeof helper !== "string" || !isAbsolute(helper)
      || helper.length > 4096 || /\p{Cc}/u.test(helper)) return false;
  try {
    const directory = lstatSync(dirname(route)), socket = lstatSync(route), executable = lstatSync(helper);
    return directory.isDirectory() && directory.uid === uid && (directory.mode & 0o022) === 0
      && socket.isSocket() && socket.uid === uid && (socket.mode & 0o022) === 0
      && executable.isFile() && [uid, 0].includes(executable.uid)
      && (executable.mode & 0o022) === 0 && (executable.mode & 0o111) !== 0;
  } catch { return false; }
}

/** Lend the socket only to the fixed read-only native peer checker. This child
 * never reads/writes telemetry, so the extension remains the native writer.
 * Cancellation kills only this newly spawned checker; output is bounded/private.
 */
function verifyPeer(socket, helper, signal) {
  return new Promise(resolve => {
    let child, timer, bytes = Buffer.alloc(0), done = false;
    const finish = accepted => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", cancel);
      if (!accepted && child?.exitCode === null && child?.signalCode === null) {
        try { child.kill("SIGKILL"); } catch { /* only our checker */ }
      }
      child?.stdout?.destroy();
      bytes.fill(0);
      resolve(accepted);
    };
    const cancel = () => finish(false);
    if (signal.aborted) { finish(false); return; }
    timer = setTimeout(cancel, 1000);
    signal.addEventListener("abort", cancel, { once: true });
    try {
      const fd = socket._handle?.fd;
      if (!Number.isSafeInteger(fd) || fd < 0) { finish(false); return; }
      child = spawn(helper, ["harness-peer"], { shell: false,
        stdio: ["ignore", "pipe", "ignore", fd], env: {} });
      child.once("error", cancel);
      child.stdout.on("error", cancel);
      child.stdout.on("data", chunk => {
        if (bytes.length + chunk.length > 128) { finish(false); return; }
        bytes = Buffer.concat([bytes, chunk]);
      });
      child.once("close", code => finish(code === 0
        && bytes.toString("utf8") === '{"protocol":"external-peer/1","verified":true}\n'));
    } catch { finish(false); }
  });
}

/** Reject duplicate and escaped-alias keys before projecting a bounded reply.
 * JSON.parse owns syntax; this finite token walk only prevents overwrites.
 */
function decode(raw) {
  const stack = [];
  for (const match of raw.matchAll(/"(?:[^"\\]|\\.)*"|[{}\[\],:]|[^{}\[\],:\s]+/gu)) {
    const token = match[0];
    if (token === "{" || token === "[") {
      if (stack.length >= 16) return;
      stack.push(token === "{" ? { keys: new Set(), key: true } : {});
    } else if (token === "}" || token === "]") stack.pop();
    else if (token === ",") { if (stack.at(-1)?.keys) stack.at(-1).key = true; }
    else if (token.startsWith('"') && stack.at(-1)?.key) {
      const object = stack.at(-1), key = JSON.parse(token);
      if (object.keys.has(key)) return;
      object.keys.add(key);
      object.key = false;
    }
  }
  return JSON.parse(raw);
}

/** Public selectors are immutable hints, never a bearer or durability receipt. */
function observer(raw, session) {
  const reply = decode(raw);
  if (!exact(reply, ["jsonrpc", "id", "result"]) || reply.jsonrpc !== "2.0"
      || reply.id !== "copilot-attach") return;
  const value = reply.result;
  const fields = ["protocol", "registered", "controls", "agent_id", "generation",
    "observer_witness", "run_id", "observer_epoch", "observer_instance",
    "external_session_id", "usage", "observer_transport", "expires_at_unix_seconds", "lease_seconds"];
  if (!exact(value, fields) || value.protocol !== "external-agent/1" || value.registered !== true
      || !Array.isArray(value.controls) || value.controls.length !== 0 || !identifier(value.agent_id)
      || !positive(value.generation) || !positive(value.run_id) || value.observer_epoch !== 1
      || !digest(value.observer_witness) || value.observer_instance !== "copilot-local-hooks-1"
      || value.external_session_id !== session || !positive(value.expires_at_unix_seconds)
      || value.lease_seconds !== 60 || value.usage !== "unavailable-source-continuity"
      || value.observer_transport !== "unavailable-curated-freshness") return;
  return Object.freeze({ external_session_id: session, generation: value.generation,
    observer_witness: value.observer_witness, run_id: value.run_id,
    observer_epoch: value.observer_epoch, agent_id: value.agent_id });
}

/** One non-retrying, content-free fixed-method exchange with a three-second cap.
 * Only an absolute route from ordinary MEZ discovery is used; there is no PATH,
 * filesystem, process-environment scraping, default-daemon or remote fallback.
 */
async function exchangeCopilot({ mez, pane, session, peerHelper } = {}, id, method, extra, project) {
  try {
    if (typeof mez !== "string" || mez.length > 8192 || !identifier(session)
        || typeof pane !== "string" || !/^%[0-9]{1,63}$/.test(pane)) return;
    const route = mez.split("\x1f", 1)[0].trim();
    if (!route.startsWith("/") || route.length > 4096 || /\p{Cc}/u.test(route)) return;
    if (!trustedPaths(route, peerHelper)) return;
    const body = JSON.stringify({ jsonrpc: "2.0", id, method,
      params: { pane_id: pane, external_session_id: session, ...extra } });
    return await new Promise(resolve => {
      let socket, timer, bytes = Buffer.alloc(0), done = false;
      const controller = new AbortController();
      const finish = value => {
        if (done) return;
        done = true;
        controller.abort();
        clearTimeout(timer);
        socket?.destroy();
        bytes.fill(0);
        resolve(value);
      };
      try {
        timer = setTimeout(() => finish(), 3000);
        socket = createConnection({ path: route });
        socket.pause();
        socket.once("error", () => finish());
        socket.once("close", () => finish());
        socket.once("connect", async () => {
          try {
            if (!await verifyPeer(socket, peerHelper, controller.signal) || done) { finish(); return; }
            socket.write(`Content-Length: ${Buffer.byteLength(body)}\r\nContent-Type: ${contentType}\r\n\r\n${body}`);
            socket.resume();
          } catch { finish(); }
        });
        socket.on("data", chunk => {
          try {
            if (done || bytes.length + chunk.length > 8448) { finish(); return; }
            const previous = bytes;
            bytes = Buffer.concat([previous, chunk]);
            previous.fill(0);
            const end = bytes.indexOf("\r\n\r\n");
            if (end < 0) { if (bytes.length > 256) finish(); return; }
            if (end > 256) { finish(); return; }
            const header = /^Content-Length: ([1-9][0-9]{0,3})\r\nContent-Type: application\/vnd\.mezzanine\.control\+json; version=1$/.exec(bytes.subarray(0, end).toString("utf8"));
            const length = header ? Number(header[1]) : 0;
            if (length === 0 || length > 8192 || bytes.length > end + 4 + length) { finish(); return; }
            if (bytes.length === end + 4 + length) {
              const raw = new TextDecoder("utf-8", { fatal: true }).decode(bytes.subarray(end + 4));
              finish(project(raw));
            }
          } catch { finish(); }
        });
      } catch { finish(); }
    });
  } catch { /* attachment loss never changes vendor behavior */ }
}

/** Read-only attachment never supplies idle proof by itself. */
export function attachCopilotObserver(options = {}) {
  return exchangeCopilot(options, "copilot-attach", "agent/external/copilot-attach", {},
    raw => observer(raw, options.session));
}

/** Send only a concrete original writer's fixed pulse, never a lease or state.
 * Public selectors are copied before I/O; errors expose no vendor/server text.
 */
export function pulseCopilotObserver(options = {}, original, observerId, sequence) {
  try {
    if (!exact(original, ["external_session_id", "generation", "observer_witness", "run_id", "observer_epoch", "agent_id"])
        || original.external_session_id !== options.session || !positive(original.generation)
        || !digest(original.observer_witness) || !positive(original.run_id)
        || original.observer_epoch !== 1 || !identifier(original.agent_id)
        || !digest(observerId) || !positive(sequence)) return Promise.resolve(undefined);
    const params = Object.freeze({ generation: original.generation,
      observer_witness: original.observer_witness, observer_id: observerId, sequence });
    return exchangeCopilot(options, "copilot-pulse", "agent/external/copilot-pulse", params, raw => {
      const reply = decode(raw);
      if (reply?.jsonrpc !== "2.0" || reply.id !== "copilot-pulse") return;
      if (exact(reply, ["jsonrpc", "id", "error"]) && [-32002, -32005, -32006].includes(reply.error?.code)) {
        return Object.freeze({ observed: false, terminal: true });
      }
      if (!exact(reply, ["jsonrpc", "id", "result"]) || !exact(reply.result, ["observed", "sequence", "changed"])
          || reply.result.observed !== true || reply.result.sequence !== sequence
          || typeof reply.result.changed !== "boolean") return;
      return Object.freeze({ observed: true, sequence, changed: reply.result.changed });
    });
  } catch { return Promise.resolve(undefined); }
}
