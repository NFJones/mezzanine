//! Exact Pi15/OpenCode13 bounded end-ACK transport, frozen historical data.
//! Never derived from active client bytes or an inspected installer receipt.

pub(super) fn bytes() -> Vec<u8> {
    let mut source =
        String::from_utf8(super::one_shot_end::bytes()).expect("frozen UTF-8 transport");
    for (old, new) in [
        (
            r####"/** Restricted ordinary-producer lifecycle and request accounting transport.
 * Loading performs no I/O. The producer owns its socket; a fixed native helper
 * verifies peer UID on a borrowed descriptor. No vendor launch, general RPC,
 * transcript reads, output pollution or fabricated usage. Accounting retries
 * restore only the same original marker/report, never vendor/provider work.
 * End uses one bounded explicit durable publication, never logical fallback or
 * re-enrollment. Lost/invalid acknowledgments remain unavailable, not proof of
 * failure to commit. This client does not provide a durable unsent-end inbox. */
import { Socket } from "node:net";
import { lstatSync } from "node:fs";
import { dirname, isAbsolute } from "node:path";
import childProcess from "node:child_process";
import { createRequestAccountingClient } from "./request_accounting.mjs";

const states = new Set(["ready", "running", "approval-wait", "input-wait", "complete", "interrupted", "failed", "background"]);
const text = (value, max) => typeof value === "string" && value.trim().length > 0
  && Buffer.byteLength(value) <= max && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
const positive = value => Number.isSafeInteger(value) && value > 0;
const contentType = "application/vnd.mezzanine.control+json; version=1";
const unavailable = () => ({ delivered: false, reason: "telemetry-unavailable" });
const peerChecks = new Set();
const cancelPeerChecks = () => { for (const cancel of [...peerChecks]) cancel(); };

/** Reprojects only inert known Pi fields; arbitrary callback content is dropped. */
function piFact(input) {
"####,
            r####"/** Restricted ordinary-producer lifecycle and request accounting transport.
 * Loading performs no I/O. The producer owns its socket; a fixed native helper
 * verifies peer UID on a borrowed descriptor. No vendor launch, general RPC,
 * transcript reads, output pollution or fabricated usage. Accounting retries
 * restore only the same original marker/report, never vendor/provider work.
 * End may repeat only its exact original selector after transport loss, with
 * two exchanges inside one monotonic horizon. It never re-enrolls or restores
 * telemetry. Invalid/exhausted ACKs remain unavailable, not proof of no commit.
 * No durable unsent-end inbox or persisted credential is provided. */
import { Socket } from "node:net";
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
"####,
        ),
        (
            r####"      && (executable.mode & 0o022) === 0 && (executable.mode & 0o111) !== 0;
  } catch { return false; }
}

/** Fixed read-only helper argv; no vendor executable or enrollment fd is used. */
function verifyPeer(socket, helper, signal) {
  return new Promise(resolve => {
    if (signal.aborted || peerChecks.size >= 16) { resolve(false); return; }
    let child;
    let bytes = Buffer.alloc(0);
    let done = false;
    const finish = accepted => {
      if (done) return;
"####,
            r####"      && (executable.mode & 0o022) === 0 && (executable.mode & 0o111) !== 0;
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
"####,
        ),
        (
            r####"      child?.stdout?.destroy();
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
"####,
            r####"      child?.stdout?.destroy();
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
"####,
        ),
        (
            r####"  let closing = false;
  let ending;
  let endPublication;
  let endOutcome;
  let endState = "not-requested";
  const lose = current => {
    if (current !== attempt) return;
    phase = "connection-lost";
    if (verifier?.attempt === current) { verifier.controller.abort(); verifier = undefined; }
    starting = undefined;
    buffer = Buffer.alloc(0);
    if (inflight) { clearTimeout(inflight.timer); inflight.resolve(undefined); inflight = undefined; }
"####,
            r####"  let closing = false;
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
"####,
        ),
        (
            r####"          || (Object.hasOwn(value, "result") === Object.hasOwn(value, "error"))) { lose(current); return; }
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
    if (pending >= 16 || disposed || closing) return Promise.resolve(unavailable());
    pending++;
    const result = serial.then(operation).catch(unavailable);
    serial = result.then(() => { pending--; }, () => { pending--; });
"####,
            r####"          || (Object.hasOwn(value, "result") === Object.hasOwn(value, "error"))) { lose(current); return; }
      buffer = Buffer.alloc(0);
      const completed = inflight;
      inflight = undefined;
      clearTimeout(completed.timer);
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
      inflight = { id, timer, resolve,
        endGeneration: method === "agent/external/source-end" ? params.generation : undefined };
      socket.write(frame);
    } catch { lose(current, "transport"); resolve(undefined); }
  });
  // Shared physical connection owner only. Verifying the peer does NOT enroll
  // or renew telemetry; end recovery can use it solely to close the frozen source.
  const connect = (current, deadline = Infinity) => (async () => {
    lossReason = undefined;
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
"####,
        ),
        (
            r####"      });
    }
    const enrollment = pendingEnrollment;
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
      const result = await exchange("agent/external/enroll", enrollment.request, current);
      if (current !== attempt || disposed || phase !== "connecting"
          || !result || result.protocol !== "external-agent/1" || result.registered !== true
          || !Array.isArray(result.controls) || result.controls.length !== 0
          || !positive(result.generation) || !positive(result.run_id) || !positive(result.observer_epoch)
"####,
            r####"      });
    }
    const enrollment = pendingEnrollment;
    const current = ++attempt;
    starting = (async () => {
      if (!await connect(current)) return false;
      const result = await exchange("agent/external/enroll", enrollment.request, current);
      if (current !== attempt || disposed || phase !== "connecting"
          || !result || result.protocol !== "external-agent/1" || result.registered !== true
          || !Array.isArray(result.controls) || result.controls.length !== 0
          || !positive(result.generation) || !positive(result.run_id) || !positive(result.observer_epoch)
"####,
        ),
        (
            r####"  const publishEnd = () => endPublication ??= (async () => {
    if (endOutcome) return endOutcome;
    closing = true;
    endState = "pending";
    const captured = handle;
    const current = attempt;
    const result = !disposed && phase === "enrolled" && captured
      && await exchange("agent/external/source-end", params(), current);
    const durable = !disposed && captured === handle && current === attempt
      && sourceEndReceipt(result, captured?.generation);
    endOutcome = Object.freeze(durable ? { delivered: true, durable: true } : unavailable());
    endState = durable ? "durable" : "unavailable";
    disposed = true;
    lose(current);
    handle = undefined;
    pendingEnrollment = undefined;
    acceptedRecovery = undefined;
    return endOutcome;
  })();
"####,
            r####"  const publishEnd = () => endPublication ??= (async () => {
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
"####,
        ),
    ] {
        assert_eq!(
            source.matches(old).count(),
            1,
            "frozen bounded-end fragment changed"
        );
        source = source.replacen(old, new, 1);
    }
    source.into_bytes()
}

#[cfg(test)]
mod tests {
    /// Whole-result digest is the deployed release, not current source.
    #[test]
    fn bootstrap_bounded_end_history_matches_deployed_bytes() {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(super::bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest,
            "531b5da02f36f7792541392323e375a5f92c6bad054678cb34ff8db37599f8e5"
        );
    }
}
