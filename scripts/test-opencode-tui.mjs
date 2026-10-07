/** Read-only client-local OpenCode association fixtures. No vendor/server API
 * mutation or native admission authority is injected by the fake client seam. */
import assert from "node:assert/strict";
import test from "node:test";
import module, { registerOpenCodeTuiObserver } from "../crates/mezzanine/src/integrations/bootstrap/opencode_tui.mjs";

const env = { MEZ: "/tmp/fixture.sock\x1fsession=a\x1fwindow=@1\x1fpane=%1\x1fprotocol=mez-control/1", MEZ_PANE: "%1" };

/** Only inspected read/subscription/lifecycle surfaces exist. Access to message,
 * part, navigation/input/approval/network API is an immediate fixture failure. */
function fixture(session = "session-a", initial = "idle", create) {
  let selected = session;
  const roots = new Map([["session-a", { id: "session-a" }], ["session-b", { id: "session-b" }], ["child", { id: "child", parentID: "session-a" }]]);
  const statuses = new Map([["session-a", initial], ["session-b", "idle"]]);
  const waits = { permission: [], question: [] };
  const handlers = new Map();
  const records = [];
  const controller = new AbortController();
  const owner = {};
  const readSession = new Proxy({ get: id => roots.get(id), status: id => statuses.get(id) ? { type: statuses.get(id) } : undefined,
    permission: () => waits.permission, question: () => waits.question }, {
    get(target, key) { assert(key in target, `forbidden state API ${String(key)}`); return target[key]; },
  });
  const api = {
    route: new Proxy({ get current() { return selected ? { name: "session", params: { sessionID: selected, prompt: "PRIVATE" } } : { name: "home" }; } }, {
      get(target, key) { assert.equal(key, "current", "route mutation attempted"); return target[key]; },
    }),
    state: { ready: true, session: readSession },
    event: { on(type, callback) { handlers.set(type, callback); return () => handlers.delete(type); } },
    lifecycle: { signal: controller.signal, onDispose(callback) { this.dispose = callback; } },
  };
  const createClient = create ?? (options => {
    const record = { options, states: [], ended: false, disconnected: false };
    records.push(record);
    let phase = "loaded";
    return {
      async start() { phase = "enrolled"; return true; },
      status() { return { phase }; },
      async presentation(state) { record.states.push(state); return { delivered: true }; },
      async end() { phase = "connection-lost"; record.ended = true; return { delivered: true }; },
      disconnect() { phase = "connection-lost"; record.disconnected = true; },
      detach() { phase = "connection-lost"; record.disconnected = true; },
    };
  });
  const install = (extra = {}) => registerOpenCodeTuiObserver(api, { owner, env, peerHelper: "/fixture/mez", createClient, ...extra });
  const emit = event => { const before = JSON.stringify(event); assert.equal(handlers.get(event.type)?.(event), undefined); assert.equal(JSON.stringify(event), before); };
  return { api, owner, install, emit, records, waits, roots, select(value) { selected = value; }, dispose() { controller.abort(); } };
}

test("TUI-only default shape and selected root exclude first/foreign/global sessions", async () => {
  assert.equal(typeof module.id, "string");
  assert.equal(typeof module.tui, "function");
  assert.equal(module.server, undefined);
  const f = fixture("session-a", "busy");
  const observer = f.install();
  try {
    f.emit({ type: "session.created", properties: { info: { id: "session-b", parentID: null } } });
    f.emit({ type: "session.status", properties: { sessionID: "session-b", status: { type: "idle" } } });
    await observer.settled();
    assert.equal(f.records.length, 1);
    assert.equal(f.records[0].options.session, "session-a");
    assert.deepEqual(f.records[0].states, ["running"]);
    f.emit({ type: "session.idle", properties: { sessionID: "session-a", text: "PRIVATE" } });
    await observer.settled();
    assert.deepEqual(f.records[0].states, ["running", "ready"]);
    assert(!JSON.stringify(f.records).includes("PRIVATE"));
  } finally { f.dispose(); }
});

test("two local clients sharing a bus retain distinct selected-session associations", async () => {
  const first = fixture("session-a", "busy");
  const second = fixture("session-b", "idle");
  const a = first.install(); const b = second.install();
  try {
    for (const f of [first, second]) {
      f.emit({ type: "session.error", properties: { sessionID: "session-a", message: "PRIVATE" } });
      f.emit({ type: "session.status", properties: { sessionID: "session-b", status: { type: "busy" } } });
    }
    await a.settled(); await b.settled();
    assert.deepEqual(first.records[0].states, ["failed"]);
    assert.deepEqual(second.records[0].states, ["running"]);
    assert.equal(first.records[0].options.session, "session-a");
    assert.equal(second.records[0].options.session, "session-b");
  } finally { first.dispose(); second.dispose(); }
});

test("route changes retire only old source; child/home routes cannot borrow it", async () => {
  const f = fixture(); const observer = f.install();
  try {
    await observer.settled();
    f.select("session-b");
    f.emit({ type: "session.updated", properties: { info: { id: "session-a" } } });
    await observer.settled();
    assert.equal(f.records[0].ended, true);
    assert.equal(f.records[1].options.session, "session-b");
    f.select("child");
    f.emit({ type: "session.status", properties: { sessionID: "child", status: { type: "busy" } } });
    await observer.settled();
    assert.equal(f.records.length, 2);
    assert.equal(f.records[1].ended, true);
  } finally { f.dispose(); }
});

test("unknown cache status does not fabricate idle and scoped disposal closes transport", async () => {
  const f = fixture("session-a", null); const observer = f.install();
  try { await observer.settled(); assert.deepEqual(f.records[0].states, []); }
  finally { f.dispose(); }
  await observer.settled();
  assert.equal(f.records[0].disconnected, true);
});

test("cached concurrent waits preserve unresolved identities without exposing prompt content", async () => {
  const f = fixture("session-a", "busy");
  f.waits.permission.push({ id: "p-a", sessionID: "session-a", prompt: "PRIVATE" }, { id: "p-b", sessionID: "session-a" });
  f.waits.question.push({ id: "q-a", sessionID: "session-a", questions: "PRIVATE" });
  const observer = f.install();
  try {
    await observer.settled();
    assert.equal(f.records[0].states.at(-1), "approval-wait");
    f.emit({ type: "permission.replied", properties: { sessionID: "session-a", requestID: "p-a" } });
    await observer.settled();
    assert.equal(f.records[0].states.at(-1), "approval-wait");
    f.emit({ type: "permission.replied", properties: { sessionID: "session-a", requestID: "p-b" } });
    await observer.settled();
    assert.equal(f.records[0].states.at(-1), "input-wait");
    f.emit({ type: "question.replied", properties: { sessionID: "session-a", requestID: "q-a", answer: "PRIVATE" } });
    await observer.settled();
    assert.equal(f.records[0].states.at(-1), "running");
    assert(!JSON.stringify(f.records).includes("PRIVATE"));
  } finally { f.dispose(); await observer.settled(); }
});

test("home scope is resource-free and does not hang settlement", async () => {
  const f = fixture(null); const observer = f.install();
  try {
    await observer.settled(); assert.equal(f.records.length, 0);
    f.select("session-a");
    f.emit({ type: "session.status", properties: { sessionID: "session-a", status: { type: "busy" } } });
    await observer.settled();
    assert.equal(f.records.length, 1, "home startup permanently disabled later selection");
  }
  finally { f.dispose(); await observer.settled(); }
});

test("not-ready startup remains subscribed until the client cache becomes usable", async () => {
  const f = fixture(); f.api.state.ready = false;
  const observer = f.install();
  try {
    await observer.settled(); assert.equal(f.records.length, 0);
    f.api.state.ready = true;
    f.emit({ type: "session.status", properties: { sessionID: "session-a", status: { type: "busy" } } });
    await observer.settled();
    assert.equal(f.records.length, 1);
  } finally { f.dispose(); await observer.settled(); }
});

test("silent SDK close cannot bypass finite paced recovery through presentation auto-start", async () => {
  let starts = 0; let phase = "loaded"; let clock = 0;
  const create = () => ({
    async start() { starts++; phase = "enrolled"; return true; },
    status() { return { phase }; },
    async presentation() { if (phase !== "enrolled") { starts++; phase = "enrolled"; } return { delivered: true }; },
    async end() { phase = "connection-lost"; return { delivered: true }; },
    disconnect() { phase = "connection-lost"; }, detach() { phase = "connection-lost"; },
  });
  const f = fixture("session-a", "idle", create); const observer = f.install({ now: () => clock });
  const event = () => f.emit({ type: "session.idle", properties: { sessionID: "session-a" } });
  try {
    await observer.settled(); assert.equal(starts, 1);
    for (let index = 0; index < 8; index++) { phase = "connection-lost"; event(); await observer.settled(); }
    assert.equal(starts, 1, "presentation bypassed delayed recovery policy");
    for (let attempt = 0; attempt < 4; attempt++) {
      clock += 4999; event(); await observer.settled(); assert.equal(starts, attempt + 1);
      clock++; event(); await observer.settled(); assert.equal(starts, attempt + 2);
      phase = "connection-lost"; event(); await observer.settled();
    }
    clock += 100000; event(); await observer.settled(); assert.equal(starts, 5, "retry budget was reset by silent loss");
  } finally { f.dispose(); await observer.settled(); }
});

test("stale asynchronous selection never publishes the old source after route change", async () => {
  const records = [];
  let release; let entered;
  const started = new Promise(resolve => { entered = resolve; });
  const create = options => {
    const record = { options, states: [], ended: false }; records.push(record);
    let phase = "loaded";
    return { async start() {
      if (records.indexOf(record) === 0) { entered(); await new Promise(resolve => { release = resolve; }); }
      phase = "enrolled"; return true;
    }, status() { return { phase }; },
    async presentation(state) { record.states.push(state); return { delivered: true }; },
    async end() { record.ended = true; phase = "connection-lost"; return { delivered: true }; },
    disconnect() { phase = "connection-lost"; }, detach() { phase = "connection-lost"; } };
  };
  const f = fixture("session-a", "busy", create); const observer = f.install();
  try {
    await started;
    f.select("session-b");
    f.emit({ type: "session.updated", properties: { info: { id: "session-b" } } });
    release(); await observer.settled();
    assert.deepEqual(records[0].states, []);
    assert.equal(records[0].ended, true);
    assert.equal(records[1].options.session, "session-b");
    assert.deepEqual(records[1].states, ["ready"]);
  } finally { f.dispose(); await observer.settled(); }
});
