/** Offline session-scoped extension tests; no credentials or vendor processes. */
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { createPiStreamExtension } from "../crates/mezzanine/src/integrations/bootstrap/pi_extension.mjs";
import { registerInheritedObserver } from "../crates/mezzanine/src/integrations/bootstrap/pi_entry.mjs";

test("candidate entry stays inert without explicit valid binding and opens only at matching start", () => {
  for (const binding of [{}, { descriptor: "4", session: "bound" },
    { descriptor: "3", session: "../private" }, { descriptor: "3", session: "bad\n" }]) {
    registerInheritedObserver({ on() { assert.fail("invalid binding registered callbacks"); } }, binding,
      () => assert.fail("factory opened a descriptor"));
  }
  const handlers = new Map();
  let opened = 0;
  const stream = new EventEmitter();
  stream.writableLength = 0;
  stream.write = () => true;
  registerInheritedObserver({ on(type, handler) {
    const list = handlers.get(type) ?? [];
    list.push(handler);
    handlers.set(type, list);
  } }, { descriptor: "3", session: "bound" }, () => {
    opened++;
    return { stream, close() { stream.emit("close"); } };
  });
  assert.equal(opened, 0);
  for (const session of ["other", "bound", "bound"]) {
    for (const handler of handlers.get("session_start")) {
      assert.equal(handler({ type: "session_start", reason: "startup" },
        { sessionManager: { getSessionId: () => session } }), undefined);
    }
  }
  assert.equal(opened, 1);
});

function fixture(open) {
  const handlers = new Map();
  const pi = new Proxy({ on(type, fn) {
    const list = handlers.get(type) ?? [];
    list.push(fn);
    handlers.set(type, list);
  } }, { get(target, key) {
    assert.equal(key, "on", "unexpected vendor mutation API");
    return target[key];
  } });
  createPiStreamExtension("bound", open)(pi);
  return (event, session = "bound") => {
    const before = JSON.stringify(event);
    for (const handler of handlers.get(event.type) ?? []) {
      assert.equal(handler(event, { sessionManager: { getSessionId: () => session } }), undefined);
    }
    assert.equal(JSON.stringify(event), before);
  };
}

test("factory does not open, session start opens once, shutdown forwards then closes", () => {
  let opened = 0;
  let closed = 0;
  const frames = [];
  const stream = new EventEmitter();
  stream.writableLength = 0;
  stream.write = (frame) => { frames.push(frame); return true; };
  const emit = fixture(() => {
    opened++;
    return { stream, close() { closed++; stream.emit("close"); } };
  });
  assert.equal(opened, 0);
  emit({ type: "agent_start" });
  assert.equal(frames.length, 0);
  emit({ type: "session_start", reason: "startup", previousSessionFile: "PRIVATE" });
  emit({ type: "agent_start", prompt: "PRIVATE" });
  emit({ type: "session_start", reason: "startup" });
  assert.equal(opened, 1);
  emit({ type: "session_shutdown", reason: "reload", targetSessionFile: "PRIVATE" });
  assert.equal(closed, 1);
  assert.deepEqual(JSON.parse(frames.at(-1)), { type: "session_shutdown", reason: "reload" });
  const count = frames.length;
  emit({ type: "agent_start" });
  emit({ type: "session_start", reason: "reload" });
  emit({ type: "session_shutdown", reason: "quit" });
  assert.equal(opened, 1);
  assert.equal(closed, 1);
  assert.equal(frames.length, count);
  assert.equal(frames.join("").includes("PRIVATE"), false);
  assert.equal(stream.listenerCount("error"), 0);
});

test("wrong sessions and opener failure stay neutral without retry or rebind", () => {
  let attempts = 0;
  const emit = fixture(() => { attempts++; throw new Error("PRIVATE"); });
  emit({ type: "session_start", reason: "startup" }, "other");
  assert.equal(attempts, 0);
  emit({ type: "session_start", reason: "startup" });
  emit({ type: "session_start", reason: "reload" });
  assert.equal(attempts, 1);
  assert.throws(() => createPiStreamExtension("PRIVATE\n", () => {}), /binding unavailable/);
});
