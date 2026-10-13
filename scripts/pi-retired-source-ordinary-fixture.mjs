/** Actual installed ordinary Pi source: retired same-ID return is unavailable.
 * Fresh observer naming cannot restore authority. No fd3/token/parent report or
 * cost injection, label fallback, failed-owner clearing, or fake fork continuation. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { readFile, writeFile } from "node:fs/promises";

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
  // Test-only fault after all real native and failed-owner assertions. Parent
  // marks its observed native EOF separately; this carries no RPC authority.
  if (process.argv[5] === "assertion-failure") {
    const deadline = Date.now() + 5000;
    while (await readFile(process.argv[6], "utf8").catch(() => "") !== "1") {
      if (Date.now() >= deadline) process.exit(2);
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    // Separate proof of reaching the intended fault, not an earlier assertion.
    await writeFile(process.argv[6], "2");
  }
  // Qualification must reject this result despite having observed socket EOF.
  assert.notEqual(process.argv[5], "assertion-failure", "intentional final child assertion failure");
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
