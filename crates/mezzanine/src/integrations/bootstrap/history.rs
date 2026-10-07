//! Immutable shipped source bytes used only for exact historical ownership.
//!
//! These snapshots are data, not active entry points. Keeping them independent of
//! current source prevents future edits from redefining accepted old manifests.
//! Fixed helper references remain generated for the installing executable only;
//! unknown historical paths/bytes never become installer authority.

/// Exact Pi entry bytes shipped in revisions 2 and 3 before ordinary activation.
pub(super) const PI_ENTRY_V3: &[u8] = br#"/**
 * Best-effort Pi directory-extension entry point.
 *
 * Only an explicitly selected session and inherited observation socket can
 * enable this observer. Environment markers are discovery hints, not daemon
 * credentials or authority. Factory loading opens no descriptor or timer.
 * A private launcher must supply descriptor 3; missing, stale or duplicate
 * binding stays neutral. Same-session reload leases the process-owned stream
 * once the old extension releases it; daemon capabilities never cross this link.
 */
import { Socket } from "node:net";
import { fstatSync } from "node:fs";
import { createPiStreamExtension } from "./pi_extension.mjs";
import { createPiBindingExtension } from "./pi_binding.mjs";

const channelOwner = Symbol.for("mezzanine.pi.observer-descriptor.v1");

/** Acquire one instance lease from a process-owned observation channel. Reload
 * releases only that lease; the fd and neutral error handler survive. Duplicate
 * loads cannot borrow an active channel, and a closed channel never reopens. */
export function acquireProcessObserverChannel(state, openStream, startReason) {
  if (state.closed || state.lease) throw new Error("Pi observer channel unavailable");
  if (startReason !== undefined && ((!state.stream && startReason !== "startup")
      || (state.stream && state.transition !== startReason))) throw new Error("Pi observer transition unavailable");
  if (!state.stream) {
    state.stream = openStream();
    state.stream.on("error", () => { state.closed = true; });
    state.stream.on("close", () => { state.closed = true; });
  }
  const lease = {};
  const epoch = (state.epoch ?? 0) + 1;
  if (!Number.isSafeInteger(epoch)) throw new Error("Pi observer epoch exhausted");
  state.epoch = epoch;
  state.lease = lease;
  state.transition = undefined;
  return { stream: state.stream, epoch, persistent: true, fail() {
    if (state.lease !== lease) return;
    state.closed = true;
    try { state.stream.end(); } catch { /* no vendor behavior change */ }
  }, close(reason) {
    if (state.lease !== lease) return;
    state.lease = undefined;
    state.transition = reason;
    if (!["reload", "new", "resume", "fork"].includes(reason)) { state.closed = true; state.stream.end(); }
  } };
}

/** Registers an explicitly bound observer without opening its stream. */
export function registerInheritedObserver(pi, binding, open) {
  if (binding?.descriptor !== "3" || typeof binding.session !== "string"
      || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,126}[A-Za-z0-9]$|^[A-Za-z0-9]$/.test(binding.session)
      || typeof open !== "function") return;
  createPiStreamExtension(binding.session, open)(pi);
}

/** Released-loader entry: missing launch markers leave the extension inert. */
export default function mezzaninePiObserver(pi) {
  if (process.env.MEZ_PI_OBSERVER_PROTOCOL === "2") {
    const binding = { descriptor: process.env.MEZ_PI_OBSERVER_FD, session: process.env.MEZ_PI_OBSERVER_SESSION };
    if (binding.descriptor !== "3" || typeof binding.session !== "string" || !/^[A-Za-z0-9_.:-]{1,128}$/.test(binding.session)) return;
    createPiBindingExtension(binding.session, (reason) => {
      const state = globalThis[channelOwner] ??= {};
      return acquireProcessObserverChannel(state, () => {
        if (!fstatSync(3).isSocket()) throw new Error("Pi observer channel unavailable");
        return new Socket({ fd: 3, readable: false, writable: true });
      }, reason);
    })(pi);
    return;
  }
  registerInheritedObserver(pi, {
    descriptor: process.env.MEZ_PI_OBSERVER_FD,
    session: process.env.MEZ_PI_OBSERVER_SESSION,
  }, () => {
    const state = globalThis[channelOwner] ??= {};
    return acquireProcessObserverChannel(state, () => {
      if (!fstatSync(3).isSocket()) throw new Error("Pi observer channel unavailable");
      return new Socket({ fd: 3, readable: false, writable: true });
    });
  });
}
"#;

/// Exact shared-client bytes shipped with Pi revision 3/OpenCode revision 2.
pub(super) const PERSISTENT_CLIENT_V1: &[u8] = br#"/** Restricted ordinary-producer lifecycle transport. Loading performs no I/O.
 * The producer owns its socket and sends allowlisted metadata directly; a fixed
 * native helper only verifies peer UID on a borrowed descriptor. No vendor
 * launch, general initialize/RPC, transcript reads, output pollution or usage
 * fabrication. Delivery failure is neutral and never replays vendor work. */
import { Socket } from "node:net";
import { lstatSync } from "node:fs";
import { dirname, isAbsolute } from "node:path";
import { spawn } from "node:child_process";

const states = new Set(["ready", "running", "approval-wait", "input-wait", "complete", "interrupted", "failed", "background"]);
const text = (value, max) => typeof value === "string" && value.trim().length > 0
  && Buffer.byteLength(value) <= max && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
const positive = value => Number.isSafeInteger(value) && value > 0;
const contentType = "application/vnd.mezzanine.control+json; version=1";
const unavailable = () => ({ delivered: false, reason: "telemetry-unavailable" });
// Only currently owned peer-check helpers are tracked. This exit listener holds
// no event-loop reference and is removed when the finite set becomes empty.
const peerChecks = new Set();
const cancelPeerChecks = () => { for (const cancel of [...peerChecks]) cancel(); };

/** Reject duplicate keys (including escaped aliases) before JSON.parse can
 * overwrite them. Depth is finite; JSON.parse owns the remaining grammar. */
function strictReply(raw) {
  const stack = [];
  for (const token of raw.matchAll(/"(?:[^"\\]|\\.)*"|[{}\[\],:]|[^{}\[\],:\s]+/gu)) {
    const value = token[0];
    if (value === "{" || value === "[") {
      if (stack.length >= 16) throw new Error("reply unavailable");
      stack.push(value === "{" ? { keys: new Set(), key: true } : {});
    } else if (value === "}" || value === "]") {
      stack.pop();
    } else if (value === ",") {
      if (stack.at(-1)?.keys) stack.at(-1).key = true;
    } else if (value.startsWith('"') && stack.at(-1)?.key) {
      const object = stack.at(-1);
      const key = JSON.parse(value);
      if (object.keys.has(key)) throw new Error("reply unavailable");
      object.keys.add(key);
      object.key = false;
    }
  }
  return JSON.parse(raw);
}

/** Nonsecret MEZ routing selects an endpoint/pane, never supplies authority. */
export function discoverPersistentRoute(env = process.env) {
  if (typeof env.MEZ !== "string" || env.MEZ.length > 4096) return;
  const fields = env.MEZ.split("\x1f");
  if (fields.length !== 5 || !isAbsolute(fields[0]) || !text(fields[0], 1024)
      || !text(env.MEZ_PANE, 64) || fields[3] !== `pane=${env.MEZ_PANE}`
      || fields[4] !== "protocol=mez-control/1") return;
  return Object.freeze({ socket: fields[0], pane: env.MEZ_PANE, encoded: fields.join("\x1f") });
}

/** Validate socket-directory ownership plus fixed-helper metadata before spawn.
 * Native helper verification remains necessary: path metadata alone cannot
 * attest the actual peer on a connected socket or repair a replaced endpoint. */
function trustedPaths(route, helper) {
  const uid = process.geteuid?.();
  if (!Number.isInteger(uid) || !isAbsolute(helper ?? "")) return false;
  try {
    const directory = lstatSync(dirname(route.socket));
    const socket = lstatSync(route.socket);
    const executable = lstatSync(helper);
    return directory.isDirectory() && directory.uid === uid && (directory.mode & 0o022) === 0
      && socket.isSocket() && socket.uid === uid && (socket.mode & 0o022) === 0
      && executable.isFile() && [uid, 0].includes(executable.uid)
      && (executable.mode & 0o022) === 0 && (executable.mode & 0o111) !== 0;
  } catch { return false; }
}

/** Fixed argv and inherited private FD are internal peer inspection only, not
 * an enrollment channel or normal vendor-launch prerequisite. Output is captured
 * and bounded; errors never escape into the vendor's stdout/stderr. */
function verifyPeer(socket, helper, signal) {
  return new Promise(resolve => {
    if (signal.aborted || peerChecks.size >= 16) { resolve(false); return; }
    let child;
    let bytes = Buffer.alloc(0);
    let done = false;
    const finish = accepted => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", cancel);
      peerChecks.delete(cancel);
      if (peerChecks.size === 0) process.off("exit", cancelPeerChecks);
      if (!accepted && child?.exitCode === null && child?.signalCode === null) {
        // SIGTERM cannot reclaim a stopped verifier. This is only the exact
        // ChildProcess we spawned for read-only peer inspection, never a vendor.
        try { child.kill("SIGKILL"); } catch {}
      }
      child?.stdout?.destroy();
      child?.unref();
      resolve(accepted);
    };
    const cancel = () => finish(false);
    const timer = setTimeout(() => finish(false), 1000);
    timer.unref();
    if (peerChecks.size === 0) process.once("exit", cancelPeerChecks);
    peerChecks.add(cancel);
    signal.addEventListener("abort", cancel, { once: true });
    try {
      // Passing the active Socket wrapper to child stdio can interfere with its
      // libuv read ownership. Borrow only its native descriptor; unsupported
      // Node/platform handle shapes fail closed without a transport fallback.
      const fd = socket._handle?.fd;
      if (!Number.isSafeInteger(fd) || fd < 0) { finish(false); return; }
      child = spawn(helper, ["harness-peer"], { shell: false,
        stdio: ["ignore", "pipe", "ignore", fd], env: {} });
      child.once("error", () => finish(false));
      if (!child.stdout || typeof child.stdout.unref !== "function") { finish(false); return; }
      child.stdout.on("error", () => finish(false));
      child.unref();
      child.stdout.unref();
      child.stdout.on("data", chunk => {
        if (bytes.length + chunk.length > 128) { finish(false); return; }
        bytes = Buffer.concat([bytes, chunk]);
      });
      child.once("close", code => finish(code === 0
        && bytes.toString("utf8") === '{"protocol":"external-peer/1","verified":true}\n'));
    } catch { finish(false); }
  });
}

/** Creates one immutable observer-instance client. Caller metadata is projected
 * now, never retained as an arbitrary object. start() alone opens resources.
 * A successor is a fresh client against the returned predecessor generation,
 * so old callback closures cannot acquire its credentials or sequence owner. */
export function createPersistentTelemetryClient(options) {
  const route = discoverPersistentRoute(options?.env);
  const metadata = {
    pane_id: route?.pane, harness: options?.harness, version: options?.version,
    external_session_id: options?.session, display_name: options?.displayName,
    observer_kind: "persistent", observer_instance: options?.instance,
  };
  if (options?.predecessor !== undefined) metadata.predecessor_generation = options.predecessor;
  const helper = options?.peerHelper;
  const valid = route && ["pi", "opencode"].includes(metadata.harness)
    && text(metadata.version, 128) && text(metadata.external_session_id, 128)
    && text(metadata.display_name, 128) && text(metadata.observer_instance, 128)
    && (metadata.predecessor_generation === undefined || positive(metadata.predecessor_generation));
  let socket;
  let phase = "loaded";
  let handle;
  let starting;
  let serial = Promise.resolve();
  let pending = 0;
  let sequence = 0;
  let requestId = 0;
  let attempt = 0;
  let inflight;
  let buffer = Buffer.alloc(0);
  let disposed = false;
  let verifier;

  const lose = current => {
    if (current !== attempt) return;
    phase = "connection-lost";
    if (verifier?.attempt === current) {
      verifier.controller.abort();
      verifier = undefined;
    }
    starting = undefined;
    buffer = Buffer.alloc(0);
    if (inflight) { clearTimeout(inflight.timer); inflight.resolve(undefined); inflight = undefined; }
    try { socket?.destroy(); } catch {}
  };
  const response = (chunk, current) => {
    if (current !== attempt) return;
    try {
      if (buffer.length + chunk.length > 8320) { lose(current); return; }
      buffer = Buffer.concat([buffer, chunk]);
      const end = buffer.indexOf("\r\n\r\n");
      if (end < 0) { if (buffer.length > 128) lose(current); return; }
      const header = buffer.subarray(0, end).toString("ascii").split("\r\n");
      if (header.length !== 2 || header[1] !== `Content-Type: ${contentType}`) { lose(current); return; }
      const match = /^Content-Length: ([1-9][0-9]{0,3})$/.exec(header[0]);
      const length = match && Number(match[1]);
      if (!length || length > 8192) { lose(current); return; }
      if (buffer.length < end + 4 + length) return;
      if (buffer.length !== end + 4 + length || !inflight) { lose(current); return; }
      const value = strictReply(new TextDecoder("utf-8", { fatal: true }).decode(buffer.subarray(end + 4)));
      if (!value || value.jsonrpc !== "2.0" || value.id !== inflight.id
          || Object.keys(value).some(key => !["jsonrpc", "id", "result", "error"].includes(key))
          || (Object.hasOwn(value, "result") === Object.hasOwn(value, "error"))) { lose(current); return; }
      buffer = Buffer.alloc(0);
      const completed = inflight;
      inflight = undefined;
      clearTimeout(completed.timer);
      completed.resolve(Object.hasOwn(value, "result") ? value.result : undefined);
    } catch { lose(current); }
  };
  const exchange = (method, params, current) => new Promise(resolve => {
    try {
      if (current !== attempt || !socket || socket.destroyed || inflight) { resolve(undefined); return; }
      if (!Number.isSafeInteger(requestId + 1)) { lose(current); resolve(undefined); return; }
      const id = `observer-${++requestId}`;
      const body = JSON.stringify({ jsonrpc: "2.0", id, method, params });
      const bytes = Buffer.byteLength(body);
      const frame = `Content-Length: ${bytes}\r\nContent-Type: ${contentType}\r\n\r\n${body}`;
      if (bytes > 8192 || socket.writableLength + Buffer.byteLength(frame) > 32768) { lose(current); resolve(undefined); return; }
      const timer = setTimeout(() => lose(current), 2000);
      timer.unref();
      inflight = { id, timer, resolve };
      socket.write(frame);
    } catch { lose(current); resolve(undefined); }
  });
  const enqueue = operation => {
    if (pending >= 16 || disposed) return Promise.resolve(unavailable());
    pending++;
    const result = serial.then(operation).catch(unavailable);
    serial = result.then(() => { pending--; }, () => { pending--; });
    return result;
  };
  const start = () => {
    if (disposed || !valid) return Promise.resolve(false);
    if (phase === "enrolled") return Promise.resolve(true);
    if (starting) return starting;
    const current = ++attempt;
    starting = (async () => {
      if (!trustedPaths(route, helper)) { lose(current); return false; }
      phase = "connecting";
      socket = new Socket();
      socket.unref();
      socket.on("error", () => lose(current));
      socket.on("close", () => lose(current));
      socket.on("data", chunk => response(chunk, current));
      const connected = await new Promise(resolve => {
        const timer = setTimeout(() => { lose(current); resolve(false); }, 1000);
        timer.unref();
        socket.once("connect", () => { clearTimeout(timer); resolve(true); });
        socket.once("error", () => { clearTimeout(timer); resolve(false); });
        socket.connect(route.socket);
      });
      if (!connected || current !== attempt || disposed) { lose(current); return false; }
      const verification = { attempt: current, controller: new AbortController() };
      verifier = verification;
      const verified = await verifyPeer(socket, helper, verification.controller.signal);
      if (verifier === verification) verifier = undefined;
      if (!verified || current !== attempt || disposed) { lose(current); return false; }
      const result = await exchange("agent/external/enroll", metadata, current);
      if (!result || result.protocol !== "external-agent/1" || result.registered !== true
          || !Array.isArray(result.controls) || result.controls.length !== 0
          || !positive(result.generation) || !positive(result.run_id) || !positive(result.observer_epoch)
          || result.observer_instance !== metadata.observer_instance || result.external_session_id !== metadata.external_session_id
          || typeof result.launch_token !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(result.launch_token)
          || result.usage !== "unavailable-source-continuity") { lose(current); return false; }
      handle = { token: result.launch_token, generation: result.generation, run: result.run_id, epoch: result.observer_epoch };
      phase = "enrolled";
      return true;
    })().catch(() => { lose(current); return false; });
    starting.then(() => { if (current === attempt && phase !== "enrolled") starting = undefined; });
    return starting;
  };
  const params = () => ({ launch_token: handle.token, generation: handle.generation,
    external_session_id: metadata.external_session_id });
  return Object.freeze({ start,
    status() { return Object.freeze({ phase, usage: "unavailable-source-continuity",
      run: handle?.run, epoch: handle?.epoch }); },
    presentation(state) {
      if (!states.has(state)) return Promise.resolve(unavailable());
      return enqueue(async () => {
        if (!await start() || !Number.isSafeInteger(sequence + 1)) return unavailable();
        const result = await exchange("agent/external/presentation", { ...params(), sequence: ++sequence, state }, attempt);
        const accepted = result && typeof result.changed === "boolean" && result.sequence === sequence;
        if (!accepted) lose(attempt);
        return accepted ? { delivered: true } : unavailable();
      });
    },
    successor(instance) {
      if (!handle || !text(instance, 128)) return;
      return createPersistentTelemetryClient({ harness: metadata.harness, version: metadata.version,
        session: metadata.external_session_id, displayName: metadata.display_name, instance,
        predecessor: handle.generation, peerHelper: helper,
        env: { MEZ: route.encoded, MEZ_PANE: route.pane } });
    },
    end() {
      if (pending >= 16 || disposed) { disposed = true; lose(attempt); return Promise.resolve(unavailable()); }
      return enqueue(async () => {
        const result = phase === "enrolled" && await exchange("agent/external/deregister", params(), attempt);
        disposed = true;
        lose(attempt);
        return result?.retired === true ? { delivered: true } : unavailable();
      });
    },
    detach() { disposed = true; lose(attempt); },
  });
}
"#;

/// Reconstructs exact shipped v2 from fixed v1 data and fixed inspected deltas.
/// It never consults current source, receipts, paths or runtime user input.
pub(super) fn persistent_client_v2() -> Vec<u8> {
    let source = String::from_utf8_lossy(PERSISTENT_CLIENT_V1);
    let source = source.replace("/** Reject duplicate keys", &format!("{PI_FACT_V2}/** Reject duplicate keys"))
        .replace("  let verifier;\n", "  let verifier;\n  let projectionMode;\n")
        .replace("      if (!states.has(state)) return Promise.resolve(unavailable());\n",
            "      if (!states.has(state) || projectionMode === \"pi\") return Promise.resolve(unavailable());\n      projectionMode = \"generic\";\n")
        .replace("    successor(instance) {\n", &format!("{PI_METHOD_V2}    successor(instance) {{\n"))
        .replace("    detach() { disposed = true; lose(attempt); },\n",
            "    // Resource cancellation retains exact immutable attempt identity for later\n    // caller-owned recovery. It never retries work or transfers a successor.\n    disconnect() { lose(attempt); },\n    detach() { disposed = true; lose(attempt); },\n");
    source.into_bytes()
}

/// Exact inert projection addition shipped in the second shared client.
const PI_FACT_V2: &str = r#"/** Reprojects only inert known Pi fields; arbitrary callback objects/content are
 * discarded before transport. Daemon Event validation remains authoritative. */
function piFact(input) {
  try {
    if (!input || typeof input !== "object" || Array.isArray(input) || !Object.hasOwn(input, "type")) return;
    const type = input.type;
    if (["agent_start", "agent_settled"].includes(type)) return { type };
    if (["session_start", "session_shutdown"].includes(type)) {
      const allowed = type === "session_start" ? ["startup", "reload", "new", "resume", "fork"] : ["quit", "reload", "new", "resume", "fork"];
      const reason = Object.hasOwn(input, "reason") ? input.reason : undefined;
      if (allowed.includes(reason)) return { type, reason };
    }
    if (["ui_prompt_start", "ui_prompt_end"].includes(type)) {
      const reason = Object.hasOwn(input, "reason") ? input.reason : undefined;
      const kind = Object.hasOwn(input, "kind") ? input.kind : undefined;
      if (reason === "ui_prompt" && ["select", "confirm", "input", "editor", "custom"].includes(kind)) {
        return { type, reason, kind };
      }
    }
    if (type === "agent_before_settle") {
      const outcome = Object.hasOwn(input, "outcome") ? input.outcome : undefined;
      if (["completed", "aborted", "error"].includes(outcome)) return { type, outcome };
    }
  } catch { /* telemetry input failure is neutral */ }
}

"#;

/// Exact fixed typed Pi operation shipped in the second shared client.
const PI_METHOD_V2: &str = r#"    piObservation(input) {
      const event = metadata.harness === "pi" && piFact(input);
      if (!event || projectionMode === "generic") return Promise.resolve(unavailable());
      projectionMode = "pi";
      return enqueue(async () => {
        if (!await start() || !Number.isSafeInteger(sequence + 1)) return unavailable();
        const result = await exchange("agent/external/pi-observation", { ...params(), sequence: ++sequence, event }, attempt);
        const accepted = result?.accepted === true && result.sequence === sequence && typeof result.retired === "boolean";
        if (!accepted) lose(attempt);
        if (accepted && result.retired) { disposed = true; lose(attempt); }
        return accepted ? { delivered: true, retired: result.retired } : unavailable();
      });
    },
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// Historical data must remain byte-identical to the shipped inspected
    /// sources, independently of later active entry/client implementation edits.
    #[test]
    fn bootstrap_history_source_snapshots_are_immutable() {
        let v2 = persistent_client_v2();
        assert_eq!(
            Sha256::digest(&v2)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "b01a4baee8c198b0edea9bf4adf03ae2c217a74c597d48502694cee69c7267d9"
        );
        for (bytes, expected) in [
            (
                PI_ENTRY_V3,
                "d8ac6138a1f2500609cb093ca27022f922a40e7f46dcc6f7a7a7a008c7f02257",
            ),
            (
                PERSISTENT_CLIENT_V1,
                "6affabd322422a5572973d2bd743086b5f6632154aa89e51b35e473aa92474a0",
            ),
        ] {
            let actual = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            assert_eq!(actual, expected);
        }
    }
}
