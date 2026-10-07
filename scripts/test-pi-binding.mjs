/** Offline v2 child binding proposals; no daemon credentials or vendor calls. */
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { createPiBindingExtension } from "../crates/mezzanine/src/integrations/bootstrap/pi_binding.mjs";
import { acquireProcessObserverChannel } from "../crates/mezzanine/src/integrations/bootstrap/pi_entry.mjs";

function fixture() {
  const stream = new EventEmitter();
  const frames = [];
  stream.writableLength = 0;
  stream.write = (value) => { frames.push(JSON.parse(value)); return true; };
  stream.end = () => stream.emit("close");
  const state = {};
  const load = () => {
    const handlers = new Map();
    createPiBindingExtension("initial", (reason) => acquireProcessObserverChannel(state, () => stream, reason))({
      on(type, fn) { const list = handlers.get(type) ?? []; list.push(fn); handlers.set(type, list); },
    });
    return (event, session = "initial") => {
      const original = JSON.stringify(event);
      for (const fn of handlers.get(event.type) ?? []) {
        assert.equal(fn(event, { sessionManager: { getSessionId: () => session } }), undefined);
      }
      assert.equal(JSON.stringify(event), original);
    };
  };
  return { stream, frames, state, load };
}

test("new/resume/fork propose bounded new epochs and reload fences old instances", () => {
  const { frames, state, load, stream } = fixture();
  const old = load();
  assert.equal(state.stream, undefined, "factory opened resources");
  old({ type: "session_start", reason: "startup" }, "foreign");
  assert.equal(frames.length, 0);
  old({ type: "session_start", reason: "startup", path: "PRIVATE" });
  let session = "initial";
  let epoch = 1;
  for (const [reason, next] of [["new", "second"], ["resume", "initial"], ["fork", "branch"]]) {
    old({ type: "agent_start", prompt: "PRIVATE" }, session);
    old({ type: "session_shutdown", reason, targetSessionFile: "PRIVATE" }, session);
    old({ type: "agent_settled" }, session);
    epoch++;
    old({ type: "session_start", reason, path: "PRIVATE" }, next);
    const last = frames.at(-1);
    assert.deepEqual(last, { session: next, epoch, event: { type: "session_start", reason } });
    session = next;
  }
  old({ type: "session_shutdown", reason: "reload" }, session);
  const replacement = load();
  replacement({ type: "session_start", reason: "reload" }, session);
  const count = frames.length;
  old({ type: "agent_start" }, session);
  assert.equal(frames.length, count);
  replacement({ type: "agent_start" }, session);
  assert.equal(frames.at(-1).epoch, epoch+1);
  replacement({ type: "session_shutdown", reason: "quit" }, session);
  assert.equal(state.closed, true);
  assert.equal(frames.some((frame) => JSON.stringify(frame).includes("PRIVATE")), false);
  assert.equal(stream.listenerCount("error"), 1);
});

test("v2 channel failure and duplicate loads cannot revive or hijack binding", () => {
  const { stream, frames, state, load } = fixture();
  const current = load();
  current({ type: "session_start", reason: "startup" });
  const duplicate = load();
  duplicate({ type: "session_start", reason: "startup" });
  const count = frames.length;
  duplicate({ type: "agent_start" });
  assert.equal(frames.length, count);
  stream.writableLength = 32768;
  current({ type: "agent_start" });
  current({ type: "session_shutdown", reason: "new" });
  assert.equal(state.closed, true);
  stream.writableLength = 0;
  current({ type: "session_start", reason: "new" }, "replacement");
  assert.equal(frames.length, count);
});
