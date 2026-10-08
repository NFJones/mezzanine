/** Offline curated-mod middleware fixtures: no vendors/providers or user config. */
import assert from "node:assert/strict";
import test from "node:test";
import { createClaudeObserver, projectClaudeEvent } from "../crates/mezzanine/src/integrations/bootstrap/claude_observer.mjs";

const forbiddenApi = new Proxy({}, { get() { throw new Error("vendor API accessed"); } });
const input = (name, fields = {}) => Object.freeze({ session_id: "bound", hook_event_name: name, ...fields });

test("pure projection needs no middleware handles and rejects inherited event names", () => {
  const event = input("UserPromptSubmit", { prompt: "PRIVATE" });
  assert.deepEqual(projectClaudeEvent("bound", "UserPromptSubmit", event), { session: "bound", event: { type: "prompt_submit" } });
  for (const name of ["constructor", "__proto__", "toString", "session.start", "turn.step", "Unknown"]) {
    assert.equal(projectClaudeEvent("bound", name, input(name)), undefined);
  }
});

test("callbacks preserve exact input/result and emit only frozen inert facts", async () => {
  const facts = [];
  const observer = createClaudeObserver("bound", fact => facts.push(fact));
  assert(Object.isFrozen(observer));
  assert.equal(facts.length, 0);
  assert.equal(observer.turnStep, undefined, "streaming/generator hooks are not ordinary middleware");
  for (const [handler, name, fields, projected] of [
    ["sessionStart", "SessionStart", { source: "startup" }, { type: "session_start", reason: "startup" }],
    ["promptSubmit", "UserPromptSubmit", {}, { type: "prompt_submit" }],
    ["notification", "Notification", { notification_type: "permission_prompt" }, { type: "permission_wait" }],
    ["stop", "Stop", { stop_hook_active: false }, { type: "stop", active: false }],
    ["stopFailure", "StopFailure", {}, { type: "stop_failure" }],
    ["sessionEnd", "SessionEnd", { reason: "clear" }, { type: "session_end", reason: "clear" }],
  ]) {
    const event = input(name, fields);
    const result = Object.freeze({ block: "PRIVATE decision", preventContinuation: true });
    let calls = 0;
    assert.equal(await observer[handler](forbiddenApi, event, original => {
      calls++;
      assert.equal(original, event);
      return result;
    }), result);
    assert.equal(calls, 1);
    assert.deepEqual(facts.at(-1), { session: "bound", event: projected });
    assert(Object.isFrozen(facts.at(-1)) && Object.isFrozen(facts.at(-1).event));
  }
  assert(!JSON.stringify(facts).includes("PRIVATE"));
});

test("main --agent is not child, but either child identifier excludes borrowed parent session", async () => {
  const facts = [];
  const observer = createClaudeObserver("bound", fact => facts.push(fact));
  for (const fields of [{ agent_id: "child" }, { agentId: "child" }, { agent_id: false },
    { agentId: 0 }, { session_id: "other" }, { hook_event_name: "Unknown" }]) {
    const event = input("UserPromptSubmit", fields);
    const result = {};
    let calls = 0;
    assert.equal(await observer.promptSubmit(forbiddenApi, event, original => { calls++; assert.equal(original, event); return result; }), result);
    assert.equal(calls, 1);
  }
  assert.equal(facts.length, 0);
  await observer.promptSubmit(forbiddenApi, input("UserPromptSubmit", { agent_type: "custom-main" }), () => ({}));
  assert.equal(facts.length, 1);
});

test("content, paths, counters and directives are never read or forwarded", async () => {
  const facts = [];
  const observer = createClaudeObserver("bound", fact => facts.push(fact));
  const event = { session_id: "bound", hook_event_name: "StopFailure" };
  for (const name of ["prompt", "transcript_path", "cwd", "message", "error", "error_details",
    "last_assistant_message", "usage", "context_tokens", "model", "permission_mode"]) {
    Object.defineProperty(event, name, { enumerable: true, get() { throw new Error("PRIVATE content read"); } });
  }
  const result = {};
  assert.equal(await observer.stopFailure(forbiddenApi, Object.freeze(event), () => result), result);
  assert.deepEqual(facts, [{ session: "bound", event: { type: "stop_failure" } }]);
});

test("metadata selectors are captured once; unknown reasons and malformed stop flags are inert", async () => {
  const facts = [];
  const observer = createClaudeObserver("bound", fact => facts.push(fact));
  const counts = new Map();
  const event = new Proxy(input("SessionStart", { source: "resume" }), {
    get(target, name) { counts.set(name, (counts.get(name) ?? 0) + 1); return target[name]; },
  });
  await observer.sessionStart(forbiddenApi, event, () => ({}));
  assert([...counts.values()].every(count => count === 1));
  for (const reason of ["startup", "resume", "clear", "compact", "fork"]) {
    await observer.sessionStart(forbiddenApi, input("SessionStart", { source: reason }), () => ({}));
  }
  const count = facts.length;
  await observer.sessionStart(forbiddenApi, input("SessionStart", { source: "PRIVATE" }), () => ({}));
  await observer.sessionEnd(forbiddenApi, input("SessionEnd", { reason: "PRIVATE" }), () => ({}));
  await observer.notification(forbiddenApi, input("Notification", { notification_type: "other" }), () => ({}));
  await observer.stop(forbiddenApi, input("Stop", { stop_hook_active: "false" }), () => ({}));
  assert.equal(facts.length, count);
});

test("downstream exceptions preserve identity and are not mistaken for observed success", async () => {
  const facts = [];
  const observer = createClaudeObserver("bound", fact => facts.push(fact));
  const error = new Error("PRIVATE downstream failure");
  for (const next of [() => { throw error; }, () => Promise.reject(error)]) {
    let calls = 0;
    await assert.rejects(observer.promptSubmit(forbiddenApi, input("UserPromptSubmit"), event => { calls++; assert.equal(event.hook_event_name, "UserPromptSubmit"); return next(); }), caught => caught === error);
    assert.equal(calls, 1);
  }
  assert.equal(facts.length, 0);
});

test("publisher exceptions, rejection, pressure and unsettled promises cannot delay/change vendor results", async () => {
  for (const enqueue of [() => { throw new Error("PRIVATE"); }, () => Promise.reject(new Error("PRIVATE")),
    () => false, () => new Promise(() => {})]) {
    const observer = createClaudeObserver("bound", enqueue);
    const result = Object.freeze({ continue: false });
    assert.equal(await observer.promptSubmit(forbiddenApi, input("UserPromptSubmit"), () => result), result);
  }
});

test("malformed session selectors and throwing metadata getters never alter downstream behavior", async () => {
  for (const id of ["", "../path", "x".repeat(129), "line\nprivate", {}, null]) {
    assert.throws(() => createClaudeObserver(id, () => {}), /binding unavailable/);
  }
  assert.throws(() => createClaudeObserver("bound", null), /binding unavailable/);
  let facts = 0;
  const observer = createClaudeObserver("bound", () => facts++);
  const event = { get session_id() { throw new Error("PRIVATE getter"); } };
  const result = {};
  assert.equal(await observer.promptSubmit(forbiddenApi, event, original => { assert.equal(original, event); return result; }), result);
  assert.equal(facts, 0);
});
