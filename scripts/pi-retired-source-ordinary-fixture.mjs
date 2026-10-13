/** Actual installed ordinary Pi source: retired same-ID return is unavailable.
 * Fresh observer naming cannot restore authority. No fd3/token/parent report or
 * cost injection, label fallback, failed-owner clearing, or fake fork continuation. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 30000);
const { default: install } = await import(pathToFileURL(process.argv[2]).href);
const handlers = new Map();
install(new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
  get(target, key) { assert.equal(key, "on"); return target[key]; },
}));
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
let session = "retired-a";
const emit = event => {
  const before = JSON.stringify(event);
  assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => session } }), undefined);
  assert.equal(JSON.stringify(event), before);
};
const settle = async () => { await globalThis[ownerKey].tail; await globalThis[ownerKey].tail; };
try {
  emit({ type: "session_start", reason: "startup" }); await settle();
  assert.equal(globalThis[ownerKey].lease.failed, false);
  emit({ type: "agent_start" });
  emit({ type: "session_shutdown", reason: "new" }); await settle();
  session = "fresh-b";
  emit({ type: "session_start", reason: "new" }); await settle();
  assert.equal(globalThis[ownerKey].lease.failed, false);
  emit({ type: "agent_start" }); emit({ type: "session_shutdown", reason: "resume" }); await settle();
  session = "retired-a";
  emit({ type: "session_start", reason: "resume" }); await settle();
  assert.equal(globalThis[ownerKey].lease.failed, true, "retired source unexpectedly reactivated");
  assert.equal(globalThis[ownerKey].client.status().usage, "unavailable-source-continuity");
  emit({ type: "turn_start", turnIndex: 0 }); await settle();
  assert.equal(globalThis[ownerKey].lease.failed, true);
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
