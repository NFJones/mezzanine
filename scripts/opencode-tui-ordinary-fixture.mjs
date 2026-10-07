/** Fake local TUI API under the available embedded Bun runtime. It loads only
 * the installed config's real TUI-only module; no vendor/provider is launched. */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 30000);
const config = JSON.parse(readFileSync(process.argv[2], "utf8"));
const reference = config.plugin.find(value => value === "./plugins/mezzanine/opencode_tui.mjs");
assert.equal(typeof reference, "string", "bootstrap omitted owned TUI registration");
const { default: module } = await import(pathToFileURL(resolve(dirname(process.argv[2]), reference)).href);
assert.equal(typeof module.tui, "function");
assert.equal(module.server, undefined);
let selected = "session-a";
const handlers = new Map();
const controller = new AbortController();
const api = { route: { get current() { return { name: "session", params: { sessionID: selected, prompt: "PRIVATE" } }; } },
  state: { ready: true, session: { get(session) { return { id: session }; },
    status() { return { type: "busy" }; }, permission() { return []; }, question() { return []; } } },
  event: { on(type, callback) { handlers.set(type, callback); return () => handlers.delete(type); } },
  lifecycle: { signal: controller.signal, onDispose(callback) { this.dispose = callback; } } };
const key = Symbol.for("mezzanine.opencode.tui-owner.v1");
const settle = async () => {
  let observed;
  do { observed = globalThis[key]?.tail; assert(observed, "TUI plugin did not observe selected root"); await observed; }
  while (observed !== globalThis[key].tail);
};
const emit = event => assert.equal(handlers.get(event.type)?.(event), undefined);
try {
  module.tui(new Proxy(api, { get(target, name) {
    assert(["route", "state", "event", "lifecycle"].includes(name), "forbidden vendor API accessed"); return target[name];
  } }));
  emit({ type: "session.created", properties: { info: { id: "foreign" } } });
  emit({ type: "session.error", properties: { sessionID: "foreign", message: "PRIVATE" } });
  await settle();
  selected = "session-b";
  emit({ type: "session.updated", properties: { info: { id: "session-b" } } });
  await settle();
  emit({ type: "session.status", properties: { sessionID: "session-b", status: { type: "busy" } } });
  await settle();
  controller.abort();
  await settle();
} finally { controller.abort(); clearTimeout(lifetime); }
