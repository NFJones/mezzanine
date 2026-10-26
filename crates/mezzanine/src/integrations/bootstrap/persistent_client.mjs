/** Restricted ordinary-producer lifecycle and request accounting transport.
 * Loading performs no I/O. The producer owns its socket; a fixed native helper
 * verifies peer UID on a borrowed descriptor. No vendor launch, general RPC,
 * transcript reads, output pollution or fabricated usage. Accounting retries
 * restore only the same original marker/report, never vendor/provider work.
 * End may repeat only its exact original selector after transport loss, with
 * two exchanges inside one monotonic horizon. It never re-enrolls or restores
 * telemetry. Invalid/exhausted ACKs remain unavailable, not proof of no commit.
 * No durable unsent-end inbox or persisted credential is provided. */
import { Socket } from "node:net";
import { randomBytes } from "node:crypto";
import { lstatSync } from "node:fs";
import { dirname, isAbsolute } from "node:path";
import childProcess from "node:child_process";
import { performance } from "node:perf_hooks";
import { createRequestAccountingClient } from "./request_accounting.mjs";

const states = new Set(["ready", "running", "approval-wait", "input-wait", "complete", "interrupted", "failed", "background"]);
const text = (value, max) => typeof value === "string" && value.trim().length > 0
  && Buffer.byteLength(value) <= max && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
const positive = value => Number.isSafeInteger(value) && value > 0;
const contentType = "application/vnd.mezzanine.control+json; version=1";
const unavailable = () => ({ delivered: false, reason: "telemetry-unavailable" });
// Only private deadlines reach this helper; callers cannot expand any budget.
const remaining = (deadline, cap) => Math.max(0, Math.min(cap, deadline - performance.now()));
const peerChecks = new Set();
const cancelPeerChecks = () => { for (const cancel of [...peerChecks]) cancel(); };

/** Reprojects only inert known Pi fields; arbitrary callback content is dropped. */
function piFact(input) {
  try {
    if (!input || typeof input !== "object" || Array.isArray(input) || !Object.hasOwn(input, "type")) return;
    const type = input.type;
    if (["agent_start", "agent_settled"].includes(type)) return { type };
    if (type === "usage_gap") {
      const reason = Object.hasOwn(input, "reason") ? input.reason : undefined;
      if (["request-readiness", "report-clock", "finalized-counters", "response-correlation", "unmatched-finalized-event", "immutable-observation-conflict", "durable-acknowledgment", "overlapping-turn"].includes(reason)) return { type, reason };
    }
    if (["session_start", "session_shutdown"].includes(type)) {
      const allowed = type === "session_start" ? ["startup", "reload", "new", "resume", "fork"] : ["quit", "reload", "new", "resume", "fork"];
      const reason = Object.hasOwn(input, "reason") ? input.reason : undefined;
      if (allowed.includes(reason)) return { type, reason };
    }
    if (["ui_prompt_start", "ui_prompt_end"].includes(type)) {
      const reason = Object.hasOwn(input, "reason") ? input.reason : undefined;
      const kind = Object.hasOwn(input, "kind") ? input.kind : undefined;
      if (reason === "ui_prompt" && ["select", "confirm", "input", "editor", "custom"].includes(kind)) return { type, reason, kind };
    }
    if (type === "agent_before_settle") {
      const outcome = Object.hasOwn(input, "outcome") ? input.outcome : undefined;
      if (["completed", "aborted", "error"].includes(outcome)) return { type, outcome };
    }
  } catch { /* telemetry input failure is neutral */ }
}

/** Reject duplicate keys/escaped aliases before JSON.parse overwrites them. */
function strictReply(raw) {
  const stack = [];
  for (const token of raw.matchAll(/"(?:[^"\\]|\\.)*"|[{}\[\],:]|[^{}\[\],:\s]+/gu)) {
    const value = token[0];
    if (value === "{" || value === "[") {
      if (stack.length >= 16) throw new Error("reply unavailable");
      stack.push(value === "{" ? { keys: new Set(), key: true } : {});
    } else if (value === "}" || value === "]") stack.pop();
    else if (value === ",") { if (stack.at(-1)?.keys) stack.at(-1).key = true; }
    else if (value.startsWith('"') && stack.at(-1)?.key) {
      const object = stack.at(-1);
      const key = JSON.parse(value);
      if (object.keys.has(key)) throw new Error("reply unavailable");
      object.keys.add(key);
      object.key = false;
    }
  }
  return JSON.parse(raw);
}

/** Nonsecret MEZ routing selects endpoint/pane, never supplies authority. */
export function discoverPersistentRoute(env = process.env) {
  if (typeof env.MEZ !== "string" || env.MEZ.length > 4096) return;
  const fields = env.MEZ.split("\x1f");
  if (fields.length !== 5 || !isAbsolute(fields[0]) || !text(fields[0], 1024)
      || !text(env.MEZ_PANE, 64) || fields[3] !== `pane=${env.MEZ_PANE}`
      || fields[4] !== "protocol=mez-control/1") return;
  return Object.freeze({ socket: fields[0], pane: env.MEZ_PANE, encoded: fields.join("\x1f") });
}

/** Path metadata precedes native peer validation; it is not peer authority. */
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

/** Fixed read-only helper argv; no vendor executable or enrollment fd is used. */
function verifyPeer(socket, helper, signal, deadline = Infinity) {
  return new Promise(resolve => {
    const wait = remaining(deadline, 1000);
    if (signal.aborted || peerChecks.size >= 16 || wait <= 0) { resolve(false); return; }
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
        try { child.kill("SIGKILL"); } catch {}
      }
      child?.stdout?.destroy();
      child?.unref();
      resolve(accepted);
    };
    const cancel = () => finish(false);
    const timer = setTimeout(() => finish(false), wait);
    timer.unref();
    if (peerChecks.size === 0) process.once("exit", cancelPeerChecks);
    peerChecks.add(cancel);
    signal.addEventListener("abort", cancel, { once: true });
    try {
      const fd = socket._handle?.fd;
      if (!Number.isSafeInteger(fd) || fd < 0) { finish(false); return; }
      child = childProcess.spawn(helper, ["harness-peer"], { shell: false,
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

/** Copies only public original-observer selectors; possession grants no native
 * source authority. Unknown/null/unsafe members fail before transport opens. */
function resumeSelector(input) {
  try {
    if (!input || typeof input !== "object" || Array.isArray(input)
        || Object.keys(input).length !== 2
        || Object.keys(input).some(key => !["observer_witness", "generation"].includes(key))) return;
    const observer_witness = Object.hasOwn(input, "observer_witness") ? input.observer_witness : undefined;
    const generation = Object.hasOwn(input, "generation") ? input.generation : undefined;
    if (typeof observer_witness !== "string" || !/^[a-f0-9]{64}$/.test(observer_witness) || !positive(generation)) return;
    return Object.freeze({ observer_witness, generation });
  } catch { /* arbitrary metadata getters cannot establish resume ownership */ }
}

/** Exact typed ordinal basis, not generic enrollment/transport success. */
function recoveryReceipt(input, kind = "same-live-source") {
  return input && typeof input === "object" && !Array.isArray(input)
    && Object.keys(input).length === 2
    && Object.keys(input).every(key => ["kind", "next_sequence"].includes(key))
    && input.kind === kind && input.next_sequence === 1;
}

/** Public selectors only: decimal text must never round through Number. */
function sourceReceipt(input) {
  try {
    if (!input || typeof input !== "object" || Array.isArray(input)
        || Object.keys(input).length !== 3
        || Object.keys(input).some(key => !["protocol", "activation_id", "observer_generation"].includes(key))) return;
    const { protocol, activation_id, observer_generation } = input;
    if (protocol !== "external-source-activation/1"
        || typeof activation_id !== "string" || !/^[a-f0-9]{64}$/.test(activation_id)
        || /^0+$/.test(activation_id) || typeof observer_generation !== "string"
        || !/^[1-9][0-9]{0,18}$/.test(observer_generation)
        || BigInt(observer_generation) > 9223372036854775807n) return;
    return Object.freeze({ protocol, activation_id, observer_generation });
  } catch { /* metadata/getters never supply native authority */ }
}

/** Exact immutable historical end proof, not logical retirement or helper exit. */
function sourceEndReceipt(input, generation) {
  return input && typeof input === "object" && !Array.isArray(input)
    && Object.keys(input).length === 4
    && Object.keys(input).every(key => ["protocol", "retired", "durable", "generation"].includes(key))
    && input.protocol === "external-source-end/1" && input.retired === true
    && input.durable === true && positive(input.generation) && input.generation === generation;
}

/** Private handle identity includes the credential; numeric generation alone
 * cannot identify a projection after an ephemeral registry is reconstructed. */
const sameHandle = (a, b) => a && b && ["token", "generation", "witness", "run", "epoch"].every(key => a[key] === b[key]);

/** Immutable observer client; successors transfer only public resume selectors,
 * never lifecycle credentials or instance-local request-accounting tickets. */
export function createPersistentTelemetryClient(options) {
  const route = discoverPersistentRoute(options?.env);
  const metadata = {
    pane_id: route?.pane, harness: options?.harness, version: options?.version,
    external_session_id: options?.session, display_name: options?.displayName,
    observer_kind: "persistent", observer_instance: options?.instance,
  };
  const predecessor = options?.predecessor;
  if (predecessor !== undefined) metadata.predecessor_generation = predecessor;
  Object.freeze(metadata);
  const suppliedResume = options?.sourceResume;
  const initialResume = suppliedResume === undefined ? undefined : resumeSelector(suppliedResume);
  const initialRun = options?.sourceRun;
  const initialEpoch = options?.sourceEpoch;
  const suppliedCheckpoint = options?.sourceReceipt;
  const initialCheckpoint = suppliedCheckpoint === undefined ? undefined : sourceReceipt(suppliedCheckpoint);
  const helper = options?.peerHelper;
  const valid = route && ["claude", "codex", "copilot", "opencode", "cursor", "pi"].includes(metadata.harness)
    && text(metadata.version, 128) && text(metadata.external_session_id, 128)
    && text(metadata.display_name, 128) && text(metadata.observer_instance, 128)
    && (metadata.predecessor_generation === undefined || positive(metadata.predecessor_generation))
    && (suppliedResume === undefined || (initialResume && positive(initialRun) && positive(initialEpoch)))
    && (suppliedCheckpoint === undefined || initialCheckpoint);
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
  let projectionMode;
  // An unknown reply retains the exact request and original predecessor/selector.
  // Only a validated receipt ends this attempt. No newest-owner/bare fallback.
  let pendingEnrollment;
  // Future requests follow the accepted projection, not the original parent's
  // predecessor. Frozen pending requests remain untouched after reply loss.
  let enrollmentMetadata = metadata;
  let acceptedRecovery;
  let acceptedRecoveryKind;
  let sourceCheckpoint = initialCheckpoint;
  let enrollmentRefusedAttempt;
  let closing = false;
  let ending;
  let endPublication;
  let endOutcome;
  let endState = "not-requested";
  let lossReason;
  const lose = (current, reason = "invalid") => {
    if (current !== attempt) return;
    // Preserve the FIRST cause: a close emitted after a protocol refusal must
    // not turn malformed peer data into permission for a transport retry.
    if (phase !== "connection-lost") lossReason = reason;
    phase = "connection-lost";
    if (verifier?.attempt === current) { verifier.controller.abort(); verifier = undefined; }
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
      const error = value.error;
      if (completed.method === "agent/external/enroll" && error && typeof error === "object" && !Array.isArray(error)
          && Object.keys(error).every(key => ["code", "message", "data"].includes(key))
          && Number.isSafeInteger(error.code) && typeof error.message === "string") enrollmentRefusedAttempt = current;
      if (completed.endGeneration !== undefined && !sourceEndReceipt(value.result, completed.endGeneration)) {
        lose(current); completed.resolve(undefined); return;
      }
      completed.resolve(Object.hasOwn(value, "result") ? value.result : undefined);
    } catch { lose(current); }
  };
  const exchange = (method, params, current, deadline = Infinity) => new Promise(resolve => {
    try {
      if (current !== attempt || !socket || inflight) { resolve(undefined); return; }
      if (socket.destroyed) { lose(current, "transport"); resolve(undefined); return; }
      if (!Number.isSafeInteger(requestId + 1)) { lose(current); resolve(undefined); return; }
      const id = `observer-${++requestId}`;
      const body = JSON.stringify({ jsonrpc: "2.0", id, method, params });
      const bytes = Buffer.byteLength(body);
      const frame = `Content-Length: ${bytes}\r\nContent-Type: ${contentType}\r\n\r\n${body}`;
      if (bytes > 8192 || socket.writableLength + Buffer.byteLength(frame) > 32768) { lose(current); resolve(undefined); return; }
      const wait = remaining(deadline, 2000);
      if (wait <= 0) { lose(current, "timeout"); resolve(undefined); return; }
      const timer = setTimeout(() => lose(current, "timeout"), wait);
      timer.unref();
      inflight = { id, timer, resolve, method,
        endGeneration: method === "agent/external/source-end" ? params.generation : undefined };
      socket.write(frame);
    } catch { lose(current, "transport"); resolve(undefined); }
  });
  // Shared physical connection owner only. Verifying the peer does NOT enroll
  // or renew telemetry; end recovery can use it solely to close the frozen source.
  const connect = (current, deadline = Infinity) => (async () => {
    lossReason = undefined;
    enrollmentRefusedAttempt = undefined;
    phase = "connecting";
    if (!positive(current) || current !== attempt || disposed || !trustedPaths(route, helper)) { lose(current); return false; }
    const wait = remaining(deadline, 1000);
    if (wait <= 0) { lose(current, "timeout"); return false; }
    const opened = new Socket();
    socket = opened;
    opened.unref();
    opened.on("error", () => lose(current, "transport"));
    opened.on("close", () => lose(current, "transport"));
    opened.on("data", chunk => response(chunk, current));
    const connected = await new Promise(resolve => {
      const finish = value => {
        clearTimeout(timer);
        opened.off("connect", ready); opened.off("error", failed); opened.off("close", failed);
        resolve(value);
      };
      const ready = () => finish(true);
      const failed = () => finish(false);
      const timer = setTimeout(() => { lose(current, "timeout"); finish(false); }, wait);
      timer.unref();
      opened.once("connect", ready); opened.once("error", failed); opened.once("close", failed);
      opened.connect(route.socket);
    });
    if (!connected || current !== attempt || disposed || phase !== "connecting") { lose(current); return false; }
    const verification = { attempt: current, controller: new AbortController() };
    verifier = verification;
    const verified = await verifyPeer(opened, helper, verification.controller.signal, deadline);
    if (verifier === verification) verifier = undefined;
    if (!verified || current !== attempt || disposed || phase !== "connecting" || remaining(deadline, 1) <= 0) { lose(current); return false; }
    return true;
  })().catch(() => { lose(current); return false; });
  const enqueue = operation => {
    if (pending >= 16 || disposed || closing) return Promise.resolve(unavailable());
    pending++;
    const result = serial.then(operation).catch(unavailable);
    serial = result.then(() => { pending--; }, () => { pending--; });
    return result;
  };
  // Existing authenticated renewal carries only a cached historical checkpoint.
  // A warm publication may still be pending; bounded paced probes never infer
  // its success or block lifecycle indefinitely. No background polling owner.
  const refreshCheckpoint = async (captured, current) => {
    const deadline = performance.now() + 2000;
    for (const delay of [0, 100, 250]) {
      if (delay) await new Promise(resolve => { const timer = setTimeout(resolve, delay); timer.unref(); });
      if (disposed || phase !== "enrolled" || current !== attempt || handle !== captured || remaining(deadline, 1) <= 0) return;
      const result = await exchange("agent/external/renew", params(), current, deadline);
      if (!result) return;
      if (result.generation !== captured.generation || !text(result.agent_id, 128) || !positive(result.expires_at_unix_seconds)) { lose(current); return; }
      if (!Object.hasOwn(result, "source_receipt")) continue;
      const checkpoint = sourceReceipt(result.source_receipt);
      if (!checkpoint || (sourceCheckpoint && (checkpoint.activation_id !== sourceCheckpoint.activation_id
          || BigInt(checkpoint.observer_generation) < BigInt(sourceCheckpoint.observer_generation)))) { lose(current); return; }
      sourceCheckpoint = checkpoint;
      return;
    }
  };
  const start = () => {
    if (disposed || !valid) return Promise.resolve(false);
    if (phase === "enrolled") return Promise.resolve(true);
    if (starting) return starting;
    if (!pendingEnrollment) {
      const selectors = handle ? resumeSelector({ observer_witness: handle.witness, generation: handle.generation }) : initialResume;
      pendingEnrollment = Object.freeze({
        request: Object.freeze({ ...enrollmentMetadata, ...(selectors ? { source_resume: selectors } : {}) }),
        selectors, previous: handle, run: handle?.run ?? initialRun, epoch: handle?.epoch ?? initialEpoch,
      });
    }
    let enrollment = pendingEnrollment;
    const current = ++attempt;
    starting = (async () => {
      if (!await connect(current)) return false;
      let result = await exchange("agent/external/enroll", enrollment.request, current);
      // An answered warm refusal is distinct from an unknown transport outcome.
      // Cold is a new explicitly typed same-source proof attempt, never initial
      // enrollment or newest-owner fallback. Freeze it across ambiguous replies.
      if (!result && !enrollment.cold && enrollment.selectors && sourceCheckpoint
          && BigInt(sourceCheckpoint.observer_generation) < 9223372036854775807n
          && enrollmentRefusedAttempt === current && phase === "connecting" && current === attempt && !disposed) {
        const { predecessor_generation, ...coldMetadata } = enrollmentMetadata;
        enrollment = Object.freeze({ ...enrollment, cold: true, checkpoint: sourceCheckpoint,
          request: Object.freeze({ ...coldMetadata, durable_source_resume: Object.freeze({ receipt: sourceCheckpoint, operation_id: randomBytes(32).toString("hex") }) }) });
        pendingEnrollment = enrollment;
        enrollmentRefusedAttempt = undefined;
        result = await exchange("agent/external/enroll", enrollment.request, current);
      }
      if (current !== attempt || disposed || phase !== "connecting"
          || !result || result.protocol !== "external-agent/1" || result.registered !== true
          || !Array.isArray(result.controls) || result.controls.length !== 0
          || !positive(result.generation) || !positive(result.run_id) || !positive(result.observer_epoch)
          || result.observer_instance !== metadata.observer_instance || result.external_session_id !== metadata.external_session_id
          || typeof result.launch_token !== "string" || !/^[A-Za-z0-9_-]{43}$/.test(result.launch_token)
          || typeof result.observer_witness !== "string" || !/^[a-f0-9]{64}$/.test(result.observer_witness)
          || result.usage !== "unavailable-source-continuity") { lose(current); return false; }
      const next = Object.freeze({ token: result.launch_token, generation: result.generation,
        witness: result.observer_witness, run: result.run_id, epoch: result.observer_epoch });
      const checkpoint = Object.hasOwn(result, "source_receipt") ? sourceReceipt(result.source_receipt) : undefined;
      if ((Object.hasOwn(result, "source_receipt") && !checkpoint) || (checkpoint && sourceCheckpoint
          && (checkpoint.activation_id !== sourceCheckpoint.activation_id
            || BigInt(checkpoint.observer_generation) < BigInt(sourceCheckpoint.observer_generation)))) { lose(current); return false; }
      const recovered = Object.hasOwn(result, "recovery");
      const unchanged = sameHandle(handle, next);
      if (recovered) {
        const kind = enrollment.cold ? "durable-native-source" : unchanged && sameHandle(acceptedRecovery, next) ? acceptedRecoveryKind : "same-live-source";
        if (!enrollment.selectors || !recoveryReceipt(result.recovery, kind)) { lose(current); return false; }
        if (enrollment.cold && (!checkpoint || checkpoint.activation_id !== enrollment.checkpoint.activation_id
            || BigInt(checkpoint.observer_generation) !== BigInt(enrollment.checkpoint.observer_generation) + 1n)) { lose(current); return false; }
        if (unchanged) {
          // A repeated already-accepted receipt cannot reset an advanced ordinal.
          if (!sameHandle(acceptedRecovery, next)) { lose(current); return false; }
        } else if (enrollment.cold) {
          if (next.epoch !== 1 || next.witness === enrollment.selectors.observer_witness
              || (enrollment.previous && next.token === enrollment.previous.token)) { lose(current); return false; }
          sequence = 0;
          acceptedRecovery = next;
          acceptedRecoveryKind = kind;
        } else {
          const epoch = enrollment.epoch + 1;
          if (!positive(epoch) || next.epoch !== epoch || next.run !== enrollment.run
              || next.witness === enrollment.selectors.observer_witness
              || (enrollment.previous && next.token === enrollment.previous.token)) { lose(current); return false; }
          sequence = 0;
          acceptedRecovery = next;
          acceptedRecoveryKind = kind;
        }
      } else if (enrollment.cold) {
        lose(current); return false;
      } else if (handle && !unchanged) {
        // Reconnect is not permission to infer a new projection from credentials.
        lose(current); return false;
      } else if (!handle && enrollment.selectors) {
        // A normal successor rotation uses its existing zero ordinal, but its
        // captured public parent expectations still constrain run/epoch/witness.
        const epoch = enrollment.epoch + 1;
        if (!positive(epoch) || next.epoch !== epoch || next.run !== enrollment.run
            || next.witness === enrollment.selectors.observer_witness) { lose(current); return false; }
      }
      handle = next;
      if (checkpoint) sourceCheckpoint = checkpoint;
      if (enrollment.cold) {
        const { predecessor_generation, ...acceptedMetadata } = enrollmentMetadata;
        enrollmentMetadata = Object.freeze(acceptedMetadata);
      }
      pendingEnrollment = undefined;
      phase = "enrolled";
      if (!checkpoint && sourceCheckpoint && enrollment.selectors && !enrollment.cold) {
        // Queue behind any operation whose start() this is executing inside;
        // never await work placed behind that same serial operation.
        void enqueue(() => refreshCheckpoint(next, current));
      }
      return phase === "enrolled" && current === attempt && !disposed;
    })().catch(() => { lose(current); return false; });
    starting.then(() => { if (current === attempt && phase !== "enrolled") starting = undefined; });
    return starting;
  };
  const params = () => ({ launch_token: handle.token, generation: handle.generation,
    external_session_id: metadata.external_session_id });
  // Runs on the existing serial owner, including directly inside a terminal Pi
  // observation. Never await an end queued BEHIND that same observation.
  const publishEnd = () => endPublication ??= (async () => {
    if (endOutcome) return endOutcome;
    closing = true;
    endState = "pending";
    const captured = handle;
    const selectors = captured && Object.freeze(params());
    const deadline = performance.now() + 5000;
    let durable = false;
    for (let exchangeIndex = 0; exchangeIndex < 2 && !disposed && captured && handle === captured
        && remaining(deadline, 1) > 0; exchangeIndex++) {
      if (exchangeIndex > 0 || phase !== "enrolled") {
        if (phase !== "connection-lost" || !["transport", "timeout"].includes(lossReason)
            || !positive(attempt + 1) || !await connect(++attempt, deadline)) break;
        phase = "ending"; // Peer verification is not renewed telemetry admission.
      }
      const current = attempt;
      const result = await exchange("agent/external/source-end", selectors, current, deadline);
      if (!disposed && captured === handle && current === attempt && remaining(deadline, 1) > 0
          && sourceEndReceipt(result, captured.generation)) { durable = true; break; }
      if (phase !== "connection-lost" || !["transport", "timeout"].includes(lossReason)) break;
    }
    endOutcome = Object.freeze(durable ? { delivered: true, durable: true } : unavailable());
    endState = durable ? "durable" : "unavailable";
    disposed = true;
    lose(attempt);
    handle = undefined;
    pendingEnrollment = undefined;
    acceptedRecovery = undefined;
    return endOutcome;
  })();
  const accounting = createRequestAccountingClient({ enqueue,
    async exchange(method, payload) {
      if (!await start()) return;
      return exchange(method, { ...params(), ...payload }, attempt);
    } });
  return Object.freeze({ start: () => closing ? Promise.resolve(false) : start(), ...accounting,
    status() { return Object.freeze({ phase, usage: accounting.accountingStatus(),
      run: handle?.run, epoch: handle?.epoch, sourceEnd: endState }); },
    captureHelperObservation(state) {
      if (phase !== "enrolled" || !states.has(state) || projectionMode === "pi") return Promise.resolve(undefined);
      const capturedHandle = handle;
      const capturedAttempt = attempt;
      projectionMode = "generic";
      return enqueue(async () => {
        if (phase !== "enrolled" || !capturedHandle || handle !== capturedHandle || attempt !== capturedAttempt
            || !Number.isSafeInteger(sequence + 1)) return unavailable();
        return Object.freeze({ operation: "helper-observe", harness: metadata.harness,
          generation: capturedHandle.generation, observer_witness: capturedHandle.witness,
          external_session_id: metadata.external_session_id,
          data: Object.freeze({ sequence: ++sequence, state }) });
      }).then(snapshot => snapshot?.operation === "helper-observe" ? snapshot : undefined);
    },
    presentation(state, connectedOnly = false) {
      if (!states.has(state) || projectionMode === "pi") return Promise.resolve(unavailable());
      projectionMode = "generic";
      return enqueue(async () => {
        if (connectedOnly ? phase !== "enrolled" : !await start()) return unavailable();
        if (!Number.isSafeInteger(sequence + 1)) return unavailable();
        const result = await exchange("agent/external/presentation", { ...params(), sequence: ++sequence, state }, attempt);
        const accepted = result && typeof result.changed === "boolean" && result.sequence === sequence;
        if (!accepted) lose(attempt);
        return accepted ? { delivered: true } : unavailable();
      });
    },
    piObservation(input) {
      const event = metadata.harness === "pi" && piFact(input);
      if (!event || projectionMode === "generic") return Promise.resolve(unavailable());
      projectionMode = "pi";
      return enqueue(async () => {
        if (!await start() || !Number.isSafeInteger(sequence + 1)) return unavailable();
        const result = await exchange("agent/external/pi-observation", { ...params(), sequence: ++sequence, event }, attempt);
        const accepted = result?.accepted === true && result.sequence === sequence && typeof result.retired === "boolean";
        if (!accepted) lose(attempt);
        if (accepted && result.retired) {
          const ended = await publishEnd();
          return { delivered: true, retired: true, durable: ended.durable === true };
        }
        return accepted ? { delivered: true, retired: result.retired } : unavailable();
      });
    },
    successor(instance) {
      if (disposed || closing || !handle || !text(instance, 128) || instance === metadata.observer_instance) return;
      return createPersistentTelemetryClient({ harness: metadata.harness, version: metadata.version,
        session: metadata.external_session_id, displayName: metadata.display_name, instance,
        predecessor: handle.generation, peerHelper: helper,
        sourceResume: { observer_witness: handle.witness, generation: handle.generation },
        sourceRun: handle.run, sourceEpoch: handle.epoch,
        sourceReceipt: sourceCheckpoint,
        env: { MEZ: route.encoded, MEZ_PANE: route.pane } });
    },
    end() {
      if (endOutcome) return Promise.resolve(endOutcome);
      if (endPublication) return endPublication;
      if (ending) return ending;
      if (disposed) {
        endState = "unavailable";
        endOutcome = Object.freeze(unavailable());
        disposed = true; lose(attempt);
        return Promise.resolve(endOutcome);
      }
      // One terminal obligation, not another ordinary queued callback. Existing
      // accepted work drains first; closing rejects any new ordinary admission.
      // This cannot grow beyond one promise per source or replenish attempts.
      ending = serial.then(publishEnd).catch(unavailable);
      closing = true;
      endState = "pending";
      return ending;
    },
    disconnect() { lose(attempt); },
    detach() { disposed = true; lose(attempt); },
  });
}
