/** Offline documented/local-schema plugin observations; no vendor processes. */
import assert from "node:assert/strict";
import test from "node:test";
import { createOpenCodeObserver } from "../crates/mezzanine/src/integrations/bootstrap/opencode_observer.mjs";

test("bounded wait history reports loss once and malformed callbacks stay neutral", async () => {
  const items = [];
  const hooks = createOpenCodeObserver("bound", item => items.push(item));
  assert.equal(await hooks.event(null), undefined);
  assert.equal(await hooks.event({get event(){throw new Error("PRIVATE");}}),undefined);
  for (let i=0;i<256;i++) {
    await hooks.event({event:{type:"question.replied",properties:{sessionID:"bound",requestID:`q${i}`}}});
  }
  assert.equal(items.length,0);
  await hooks.event({event:{type:"question.asked",properties:{sessionID:"bound",id:"overflow"}}});
  assert.deepEqual(items,[{session:"bound",kind:"unavailable",reason:"wait-capacity"}]);
  await hooks.event({event:{type:"session.status",properties:{sessionID:"bound",status:{type:"busy"}}}});
  assert.equal(items.length,1);
});

test("overlapping and stale wait replies cannot clear pending requests", async () => {
  const items = [];
  const hooks = createOpenCodeObserver("bound", item => items.push(item));
  const emit = (type, extra = {}) => hooks.event({event:{type,properties:{sessionID:"bound",...extra}}});
  await emit("session.status", {status:{type:"busy"}});
  await emit("permission.asked", {id:"a"});
  await emit("permission.asked", {id:"b"});
  await emit("question.asked", {id:"q"});
  await emit("permission.replied", {requestID:"a"});
  assert.equal(items.at(-1).state,"approval-wait");
  const count = items.length;
  await emit("permission.replied", {requestID:"old"});
  await emit("permission.replied", {requestID:"a"});
  await emit("permission.asked", {id:"a"});
  assert.equal(items.length,count,"stale/duplicate event changed wait ownership");
  await emit("permission.replied", {requestID:"b"});
  assert.equal(items.at(-1).state,"input-wait");
  await emit("question.rejected", {requestID:"q"});
  assert.equal(items.at(-1).state,"running");
  await emit("question.replied", {requestID:"early"});
  const prior = items.length;
  await emit("question.asked", {id:"early"});
  assert.equal(items.length,prior,"reordered resolved request reopened wait");
});

test("prototype names are not allowlisted status or event values", async () => {
  const items = [];
  const hooks = createOpenCodeObserver("bound", item => items.push(item));
  for (const name of ["toString","__proto__","constructor","hasOwnProperty"]) {
    await hooks.event({event:{type:"session.status",properties:{sessionID:"bound",status:{type:name}}}});
    await hooks.event({event:{type:name,properties:{sessionID:"bound"}}});
  }
  assert.equal(items.length,0);
});

test("OpenCode callbacks export only bound-root facts and remain neutral", async () => {
  const items = [];
  const hooks = createOpenCodeObserver("ses_bound", (value) => items.push(value));
  assert.deepEqual(Object.keys(hooks), ["event"]);
  for (const event of [
    { type:"session.created", properties:{info:{id:"ses_bound",title:"PRIVATE",directory:"PRIVATE"}} },
    { type:"session.status", properties:{sessionID:"ses_bound",status:{type:"retry",message:"PRIVATE"}} },
    { type:"permission.asked", properties:{sessionID:"ses_bound",id:"permission",patterns:["PRIVATE"]} },
    { type:"permission.replied", properties:{sessionID:"ses_bound",requestID:"permission"} },
    { type:"question.asked", properties:{sessionID:"ses_bound",id:"question",questions:["PRIVATE"]} },
    { type:"question.replied", properties:{sessionID:"ses_bound",requestID:"question"} },
    { type:"session.idle", properties:{sessionID:"ses_bound"} },
    { type:"session.error", properties:{sessionID:"ses_bound",error:{message:"PRIVATE"}} },
    { type:"session.deleted", properties:{info:{id:"ses_bound",directory:"PRIVATE"}} },
  ]) {
    const before = JSON.stringify(event);
    assert.equal(await hooks.event({event}), undefined);
    assert.equal(JSON.stringify(event), before);
  }
  assert.deepEqual(items.map((item) => item.state), ["ready","running","approval-wait","running","input-wait","running","ready","failed","retire"]);
  assert(!JSON.stringify(items).includes("PRIVATE"));
  const prior = items.length;
  await hooks.event({event:{type:"session.updated",properties:{info:{id:"ses_bound",title:"PRIVATE"}}}});
  await hooks.event({event:{type:"session.created",properties:{info:{id:"ses_bound",parentID:"parent"}}}});
  await hooks.event({event:{type:"session.status",properties:{sessionID:"child",status:{type:"busy"}}}});
  await hooks.event({event:{type:"session.status",properties:{sessionID:"ses_bound",status:{type:"unknown"}}}});
  assert.equal(items.length, prior);
});

test("metadata updates and wait replies preserve running activity", async () => {
  const items = [];
  const hooks = createOpenCodeObserver("bound", item => items.push(item));
  for (const type of ["session.status","permission.asked","session.updated","permission.replied","question.asked","question.replied"]) {
    await hooks.event({event:{type,properties:{sessionID:"bound",status:{type:"busy"},id:type.startsWith("question")?"q":"p",requestID:type.startsWith("question")?"q":"p",info:{id:"bound",title:"PRIVATE"}}}});
  }
  assert.deepEqual(items.map(item => item.state),["running","approval-wait","running","input-wait","running"]);
  const before = items.length;
  await hooks.event({event:{type:"permission.updated",properties:{sessionID:"bound"}}});
  assert.equal(items.length,before,"ambiguous permission metadata fabricated human wait");
});

test("completed messages use one allowlisted source, preserving repeat identity", async () => {
  const items = [];
  const hooks = createOpenCodeObserver("ses_bound", (value) => items.push(value));
  const info = {role:"assistant",id:"msg_1",sessionID:"ses_bound",providerID:"provider",modelID:"model",
    time:{completed:1000},tokens:{input:10,output:4,reasoning:2,cache:{read:3,write:5}},
    path:{cwd:"PRIVATE"},content:"PRIVATE",cost:123,error:{message:"PRIVATE"}};
  for (let i=0;i<2;i++) assert.equal(await hooks.event({event:{type:"message.updated",properties:{info}}}), undefined);
  assert.deepEqual(items[0],items[1]);
  assert.equal(items[0].kind,"usage");
  assert(!JSON.stringify(items).includes("PRIVATE"));
  for (const changed of [{...info,time:{}},{...info,role:"user"},{...info,sessionID:"foreign"},
    {...info,tokens:{...info.tokens,reasoning:undefined}},{...info,tokens:{...info.tokens,input:Number.MAX_SAFE_INTEGER}},
    {...info,tokens:{...info.tokens,output:1.5}}]) {
    await hooks.event({event:{type:"message.updated",properties:{info:changed}}});
  }
  await hooks.event({event:{type:"message.part.updated",properties:{sessionID:"ses_bound",tokens:info.tokens}}});
  assert.equal(items.length,2);
  const failed = createOpenCodeObserver("ses_bound",()=>{throw new Error("queue unavailable");});
  assert.equal(await failed.event({event:{type:"session.idle",properties:{sessionID:"ses_bound"}}}),undefined);
});
