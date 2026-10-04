/** Package-independent offline observer tests: no credentials or provider work. */
import assert from "node:assert/strict";
import test from "node:test";
import { createPiObserver } from "../crates/mezzanine/src/integrations/bootstrap/pi_observer.mjs";
import { EventEmitter } from "node:events";
import { createObserverStreamSink } from "../crates/mezzanine/src/integrations/bootstrap/pi_observer_stream.mjs";

test("stream sink forwards only facts and disables writes after backpressure", () => {
  const stream = new EventEmitter();
  const frames = [];
  stream.writableLength = 0;
  stream.write = (frame) => { frames.push(frame); return frames.length < 2; };
  const sink = createObserverStreamSink(stream, "bound");
  assert.equal(sink.enqueue({ session: "other", event: { type: "agent_start" } }), false);
  assert.equal(sink.enqueue({ session: "bound", event: { type: "agent_start", prompt: "PRIVATE" } }), true);
  assert.equal(sink.enqueue({ session: "bound", event: { type: "agent_settled", outcome: "PRIVATE" } }), true);
  assert.equal(sink.enqueue({ session: "bound", event: { type: "agent_start" } }), false);
  assert.deepEqual(frames, ['{"type":"agent_start"}\n', '{"type":"agent_settled"}\n']);
  sink.dispose();
  stream.emit("error", new Error("PRIVATE"));
  stream.emit("close");
  sink.releaseAfterClose();
  assert.equal(stream.listenerCount("error"), 0);
});

test("stream sink errors and finite write budget never affect vendor results", () => {
  for (const mode of ["full", "throw", "closed"]) {
    const stream = new EventEmitter();
    let writes = 0;
    stream.writableLength = mode === "full" ? 32768 : 0;
    stream.write = () => { writes++; throw new Error("PRIVATE"); };
    const sink = createObserverStreamSink(stream, "bound");
    if (mode === "closed") stream.emit("close");
    const { handlers, ctx } = fixture(sink.enqueue);
    assert.equal(handlers.get("agent_start")({ type: "agent_start", prompt: "PRIVATE" }, ctx), undefined);
    assert.equal(handlers.get("agent_start")({ type: "agent_start" }, ctx), undefined);
    assert.equal(writes, mode === "throw" ? 1 : 0);
    stream.emit("error", new Error("PRIVATE"));
    stream.emit("close");
    sink.releaseAfterClose();
  }
});

function fixture(enqueue) {
  const handlers = new Map();
  const pi = new Proxy({ on(type, handler) { handlers.set(type, handler); } }, {
    get(target, key) {
      assert.equal(key, "on", "observer attempted a vendor mutation API");
      return target[key];
    },
  });
  createPiObserver("bound", enqueue)(pi);
  const ctx = { sessionManager: { getSessionId: () => "bound" } };
  return { handlers, ctx };
}

test("only inert facts leave callbacks and all callbacks stay neutral", () => {
  const observations = [];
  const { handlers, ctx } = fixture((item) => observations.push(item));
  assert.equal(observations.length, 0, "factory must only register callbacks");
  assert.equal(handlers.has("agent_end"), false);
  assert.equal(handlers.has("turn_end"), false);
  assert.equal(handlers.has("message_end"), false);
  for (const event of [
    { type: "session_start", reason: "startup", previousSessionFile: "PRIVATE" },
    { type: "agent_start", prompt: "PRIVATE" },
    { type: "ui_prompt_start", reason: "ui_prompt", kind: "confirm", title: "PRIVATE" },
    { type: "ui_prompt_end", reason: "ui_prompt", kind: "confirm", answer: "PRIVATE" },
    { type: "agent_before_settle", outcome: "error", continue: true, context: "PRIVATE" },
    { type: "agent_settled", outcome: "completed", messages: ["PRIVATE"] },
    { type: "session_shutdown", reason: "reload", targetSessionFile: "PRIVATE" },
  ]) {
    const before = JSON.stringify(event);
    assert.equal(handlers.get(event.type)(event, ctx), undefined);
    assert.equal(JSON.stringify(event), before);
  }
  assert.equal(observations.length, 7);
  assert.equal(JSON.stringify(observations).includes("PRIVATE"), false);
  assert.deepEqual(observations[5].event, { type: "agent_settled" });
  assert(observations.every((item) => Object.isFrozen(item) && Object.isFrozen(item.event)));
});

test("wrong sessions, stale contexts and queue errors cannot mutate callback results", () => {
  let count = 0;
  const { handlers, ctx } = fixture(() => { count++; throw new Error("PRIVATE"); });
  const event = { type: "agent_start", session_id: "bound" };
  assert.equal(handlers.get(event.type)(event, { sessionManager: { getSessionId: () => "other" } }), undefined);
  assert.equal(handlers.get(event.type)(event, { get sessionManager() { throw new Error("PRIVATE"); } }), undefined);
  assert.equal(count, 0);
  assert.equal(handlers.get(event.type)(event, ctx), undefined);
  assert.equal(count, 1);
  assert.equal(handlers.get("session_start")({ reason: "quit" }, ctx), undefined);
  assert.equal(handlers.get("ui_prompt_start")({ reason: "ui_prompt", kind: "unknown" }, ctx), undefined);
  assert.equal(count, 1);
  assert.throws(() => createPiObserver("PRIVATE\n", () => {}), /binding unavailable/);
});
