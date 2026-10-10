/** Original-observer request accounting for code-owned curated SDK adapters.
 * This pure module performs no I/O on load and uses no Node/DOM globals. The
 * caller supplies only a fixed-helper runner returning captured public stdout
 * and an exact original-lifetime predicate; no generic RPC or newest lookup is
 * exposed. Stop/owner loss is permanent, including queued and late replies.
 * Already known durable ACKs remain local evidence, never an expense rollback.
 * This transport primitive alone installs no vendor request/counter producer. */
import { createRequestAccountingClient } from './request_accounting.mjs';

const identifier = value => typeof value === "string" && /^[A-Za-z0-9._:-]{1,128}$/.test(value);
const positive = value => Number.isSafeInteger(value) && value > 0;
const digest = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const unavailable = () => Object.freeze({ delivered: false, durable: false, reason: "telemetry-unavailable" });

/** Capture each original public selector once; private/unknown fields are inert. */
function observerSnapshot(value) {
  try {
    const fields = ["external_session_id", "generation", "observer_witness", "run_id", "observer_epoch", "agent_id"];
    if (!value || typeof value !== "object" || Array.isArray(value)
        || Object.keys(value).length !== fields.length || Object.keys(value).some(key => !fields.includes(key))) return;
    const snapshot = { external_session_id: value.external_session_id, generation: value.generation,
      observer_witness: value.observer_witness, run_id: value.run_id,
      observer_epoch: value.observer_epoch, agent_id: value.agent_id };
    if (!identifier(snapshot.external_session_id) || !identifier(snapshot.agent_id)
        || !positive(snapshot.generation) || !positive(snapshot.run_id) || !positive(snapshot.observer_epoch)
        || !digest(snapshot.observer_witness)) return;
    return Object.freeze(snapshot);
  } catch { /* malformed getters cannot publish source identity */ }
}

/** Decode bounded JSON without duplicate or escaped-alias keys. JSON.parse owns
 * grammar; the finite token walk prevents overwrite before typed projection. */
function decode(raw) {
  if (typeof raw !== "string" || raw.length > 4096) return;
  let bytes = 0;
  for (const character of raw) {
    const point = character.codePointAt(0);
    bytes += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
    if (bytes > 4096) return;
  }
  const stack = [];
  for (const token of raw.matchAll(/"(?:[^"\\]|\\.)*"|[{}\[\],:]|[^{}\[\],:\s]+/gu)) {
    const value = token[0];
    if (value === "{" || value === "[") {
      if (stack.length >= 16) return;
      stack.push(value === "{" ? { keys: new Set(), key: true } : {});
    } else if (value === "}" || value === "]") stack.pop();
    else if (value === ",") { if (stack.at(-1)?.keys) stack.at(-1).key = true; }
    else if (value.startsWith('"') && stack.at(-1)?.key) {
      const object = stack.at(-1), key = JSON.parse(value);
      if (object.keys.has(key)) return;
      object.keys.add(key);
      object.key = false;
    }
  }
  return JSON.parse(raw);
}

/** Return only the fixed helper's exact public readiness or durability schema. */
function receipt(raw, operation, requestId) {
  try {
    const value = decode(raw);
    const exact = (object, fields) => object && typeof object === "object" && !Array.isArray(object)
      && Object.keys(object).length === fields.length && Object.keys(object).every(key => fields.includes(key));
    if (operation === "curated-usage-begin") {
      if (!exact(value, ["protocol", "accepted", "request_id", "admitted_at", "source_witness", "origin", "controls"])
          || value.protocol !== "external-request/1" || value.accepted !== true || value.request_id !== requestId
          || !positive(value.admitted_at) || !digest(value.source_witness)
          || !["project", "unattributed"].includes(value.origin)
          || !Array.isArray(value.controls) || value.controls.length !== 0) return;
      return Object.freeze({ protocol: "external-request/1", accepted: true, request_id: requestId,
        admitted_at: value.admitted_at, source_witness: value.source_witness, origin: value.origin,
        controls: Object.freeze([]) });
    }
    if (!exact(value, ["accepted", "durable", "applied", "revision", "coverage"])
        || value.accepted !== true || value.durable !== true || typeof value.applied !== "boolean"
        || !positive(value.revision) || !exact(value.coverage, ["source", "reasoning_known"])
        || value.coverage.source !== "external-reported" || typeof value.coverage.reasoning_known !== "boolean") return;
    return Object.freeze({ accepted: true, durable: true, applied: value.applied, revision: value.revision,
      coverage: Object.freeze({ source: "external-reported", reasoning_known: value.coverage.reasoning_known }) });
  } catch { /* malformed/private output is unavailable, never user-visible */ }
}

/** Bind once to the caller's original observer. The queue bounds operations,
 * while the shared core owns the 128-ticket and same-report retry contracts. */
export function createCuratedRequestAccountingClient({ observer, isCurrent, send } = {}) {
  const original = observerSnapshot(observer);
  let stopped = !original || typeof isCurrent !== "function" || typeof send !== "function";
  let pending = 0;
  let serial = Promise.resolve();
  const current = () => {
    if (stopped) return false;
    try { if (isCurrent() === true) return true; } catch { /* owner unavailable */ }
    stopped = true;
    return false;
  };
  const enqueue = operation => {
    if (!current() || pending >= 16) return Promise.resolve(unavailable());
    pending++;
    const result = serial.then(() => current() ? operation() : unavailable()).catch(unavailable);
    serial = result.then(() => { pending--; }, () => { pending--; });
    return result;
  };
  const accounting = createRequestAccountingClient({ enqueue,
    async exchange(method, payload) {
      const operation = method === "agent/external/usage-begin" ? "curated-usage-begin"
        : method === "agent/external/usage-end" ? "curated-usage-end" : undefined;
      if (!operation || !current()) return;
      const capsule = Object.freeze({ operation, external_session_id: original.external_session_id,
        generation: original.generation, observer_witness: original.observer_witness, ...payload });
      try {
        const raw = await send(capsule);
        if (!current()) return;
        return receipt(raw, operation, payload.request_id);
      } catch { /* fixed-helper failure never retries vendor/provider work */ }
    } });
  return Object.freeze({ ...accounting, stop() { stopped = true; } });
}
