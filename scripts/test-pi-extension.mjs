/** Offline session-scoped extension tests; no credentials or vendor processes. */
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { createPiStreamExtension } from "../crates/mezzanine/src/integrations/bootstrap/pi_extension.mjs";
import { registerInheritedObserver, acquireProcessObserverChannel } from "../crates/mezzanine/src/integrations/bootstrap/pi_entry.mjs";

test("same-session reload retains a process channel and fences the old observer", () => {
  const stream = new EventEmitter();
  const frames = [];
  stream.writableLength = 0;
  stream.write = (frame) => { frames.push(JSON.parse(frame)); return true; };
  // A process owner, not an extension instance, retains this stream. Its
  // permanent neutral error listener makes per-instance listener retirement safe.
  stream.on("error", () => {});
  let open = false;
  const opener = () => {
    assert.equal(open, false, "duplicate observer borrowed live stream");
    open = true;
    return { stream, persistent: true, close(reason) {
      assert(["reload", "quit"].includes(reason), "extension did not preserve teardown reason");
      open = false;
      if (reason !== "reload") stream.emit("close");
    } };
  };
  const old = fixture(opener);
  old({ type: "session_start", reason: "startup" });
  old({ type: "agent_start" });
  old({ type: "session_shutdown", reason: "reload" });
  const replacement = fixture(opener);
  replacement({ type: "session_start", reason: "reload" });
  replacement({ type: "agent_start" });
  const count = frames.length;
  old({ type: "agent_start" });
  assert.equal(frames.length, count);
  assert.deepEqual(frames.map((frame) => frame.type),
    ["session_start", "agent_start", "session_shutdown", "session_start", "agent_start"]);
  replacement({ type: "session_shutdown", reason: "quit" });
  assert.equal(stream.listenerCount("error"), 1);
});

test("production channel owner reuses the descriptor only after reload release", () => {
  const stream = new EventEmitter();
  let opens = 0;
  let ends = 0;
  stream.end = () => { ends++; stream.emit("close"); };
  const state = {};
  const open = () => { opens++; return stream; };
  const first = acquireProcessObserverChannel(state, open);
  assert.throws(() => acquireProcessObserverChannel(state, open), /unavailable/);
  first.close("reload");
  const second = acquireProcessObserverChannel(state, open);
  first.close("quit");
  assert.equal(ends, 0, "stale instance closed replacement channel");
  second.close("quit");
  assert.equal(opens, 1);
  assert.equal(ends, 1);
  assert.throws(() => acquireProcessObserverChannel(state, open), /unavailable/);
  stream.emit("error", new Error("late write"));
});

test("failed process sink cannot revive after dropping reload shutdown", () => {
  for (const mode of ["backpressure", "budget", "throw"]) {
    const stream = new EventEmitter();
    const frames = [];
    let failing = false;
    stream.writableLength = 0;
    stream.write = (frame) => {
      if (failing && mode === "throw") throw new Error("write unavailable");
      frames.push(JSON.parse(frame));
      return !(failing && mode === "backpressure");
    };
    stream.end = () => stream.emit("close");
    const state = {};
    const opener = () => acquireProcessObserverChannel(state, () => stream);
    const old = fixture(opener);
    old({ type: "session_start", reason: "startup" });
    failing = true;
    if (mode === "budget") stream.writableLength = 32768;
    old({ type: "agent_start" });
    old({ type: "session_shutdown", reason: "reload" });
    const count = frames.length;
    failing = false;
    stream.writableLength = 0;
    const replacement = fixture(opener);
    replacement({ type: "session_start", reason: "reload" });
    replacement({ type: "agent_start" });
    assert.equal(state.closed, true);
    assert.equal(frames.length, count, `${mode} revived telemetry without shutdown`);
  }
});

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
