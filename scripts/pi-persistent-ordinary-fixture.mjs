/** Fake ordinary Pi process loading the real installed default entry. Only
 * on() callbacks/sessionManager IDs are provided; no descriptor3/token/wrapper. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 30000);
const { default: install } = await import(pathToFileURL(process.argv[2]).href);
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
const load = () => {
  const handlers = new Map();
  install(new Proxy({ on(type, fn) { handlers.set(type, fn); } }, {
    get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; },
  }));
  return (event, session = "session-a") => {
    const before = JSON.stringify(event);
    assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => session } }), undefined);
    assert.equal(JSON.stringify(event), before);
  };
};
const settle = async () => {
  const owner = globalThis[ownerKey];
  assert(owner?.tail, "installed ordinary entry did not enqueue a genuine start");
  await owner.tail;
  assert.equal(owner.client.status().usage, "unavailable-source-continuity");
  assert.equal(owner.lease?.failed ?? false, false, "ordinary Pi telemetry unavailable");
};
try {
  const original = load();
  const duplicate = load();
  assert.equal(globalThis[ownerKey], undefined, "factory opened process resources");
  original({ type: "session_start", reason: "startup", path: "PRIVATE" });
  duplicate({ type: "session_start", reason: "startup" });
  duplicate({ type: "agent_start", prompt: "PRIVATE" });
  original({ type: "agent_start", prompt: "PRIVATE" }, "child-session");
  original({ type: "agent_start", prompt: "PRIVATE" });
  original({ type: "ui_prompt_start", reason: "ui_prompt", kind: "select", prompt: "PRIVATE" });
  original({ type: "ui_prompt_end", reason: "ui_prompt", kind: "select", answer: "PRIVATE" });
  original({ type: "agent_before_settle", outcome: "completed", message: "PRIVATE" });
  original({ type: "agent_settled" });
  await settle();
  const run = globalThis[ownerKey].client.status().run;
  original({ type: "session_shutdown", reason: "reload" });
  const replacement = load();
  replacement({ type: "session_start", reason: "reload" });
  original({ type: "agent_start" });
  replacement({ type: "agent_start" });
  await settle();
  assert.equal(globalThis[ownerKey].client.status().run, run);
  assert.equal(globalThis[ownerKey].client.status().epoch, 2);
  let session = "session-a";
  for (const [reason, next] of [["new", "session-b"], ["resume", "session-a"], ["fork", "branch"]]) {
    replacement({ type: "session_shutdown", reason }, session);
    replacement({ type: "session_start", reason }, next);
    await settle();
    assert.equal(globalThis[ownerKey].client.status().epoch, 1);
    session = next;
  }
  replacement({ type: "agent_start" }, session);
  replacement({ type: "agent_before_settle", outcome: "completed" }, session);
  replacement({ type: "agent_settled" }, session);
  await settle();
  replacement({ type: "session_shutdown", reason: "quit" }, session);
  await globalThis[ownerKey].tail;
  assert.equal(globalThis[ownerKey].closed, true);
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
