/** Installed ordinary Pi entry -> real native readiness -> gap before retire.
 * No parent-supplied ticket/token/report, vendor launch, fd3 or synthetic cost. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 30000);
const { default: install } = await import(pathToFileURL(process.argv[2]).href);
const handlers = new Map();
install({ on(type, callback) { handlers.set(type, callback); } });
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
let session = "loss-a";
const emit = event => assert.equal(handlers.get(event.type)?.(event,
  { sessionManager: { getSessionId: () => session } }), undefined);
const settle = async () => { await globalThis[ownerKey].tail; await globalThis[ownerKey].tail; };
try {
  emit({ type: "session_start", reason: "startup" }); await settle();
  emit({ type: "agent_start" }); emit({ type: "turn_start", turnIndex: 0 }); await settle();
  // The request has native readiness but deliberately no finalized outcome.
  emit({ type: "session_shutdown", reason: "new" }); await settle();
  assert.equal(globalThis[ownerKey].usageGap, "finalized-counters");
  assert.equal(globalThis[ownerKey].client.status().phase, "connection-lost");
  session = "loss-b";
  emit({ type: "session_start", reason: "new" }); await settle();
  assert.equal(globalThis[ownerKey].lease.failed, false, "gap was sent after retirement");
  emit({ type: "agent_start" }); emit({ type: "turn_start", turnIndex: 0 }); await settle();
  emit({ type: "session_shutdown", reason: "quit" }); await settle();
  assert.equal(globalThis[ownerKey].closed, true);
  assert.equal(globalThis[ownerKey].client.status().usage, "unavailable-source-continuity");
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
