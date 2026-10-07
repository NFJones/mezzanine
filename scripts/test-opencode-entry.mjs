/** Offline plugin entry/writer checks; no user config, daemon or vendor runs. */
import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";
import { createOpenCodeStreamPlugin } from "../crates/mezzanine/src/integrations/bootstrap/opencode_entry.mjs";

test("entry opens only after matched root metadata and exports no content", async () => {
  const frames = [];
  const stream = new EventEmitter();
  stream.writableLength = 0;
  stream.write = frame => { frames.push(JSON.parse(frame)); return true; };
  stream.end = () => stream.emit("close");
  let opens = 0;
  const plugin = createOpenCodeStreamPlugin({descriptor:"3",session:"bound"},()=>{opens++;return stream;});
  assert.equal(opens,0);
  for (const info of [{id:"child",parentID:"bound"},{id:"bound",parentID:"parent"},{id:"other"}]) {
    assert.equal(await plugin.event({event:{type:"session.created",properties:{info}}}),undefined);
  }
  assert.equal(opens,0);
  await plugin.event({event:{type:"session.updated",properties:{info:{id:"bound",title:"PRIVATE",directory:"PRIVATE"}}}});
  await plugin.event({event:{type:"session.status",properties:{sessionID:"bound",status:{type:"busy"}}}});
  assert.equal(opens,1);
  assert.deepEqual(frames,[{session:"bound",kind:"start"},{session:"bound",kind:"status",state:"running"}]);
  assert(!JSON.stringify(frames).includes("PRIVATE"));
  await plugin.event({event:{type:"session.status",properties:{sessionID:"foreign",status:{type:"busy"}}}});
  assert.equal(frames.length,2);
});

test("fresh selection ignores preexisting metadata and failed streams do not reopen", async () => {
  const stream = new EventEmitter();
  stream.writableLength = 0;
  const frames = [];
  let available = true;
  stream.write = frame => { frames.push(JSON.parse(frame)); return available; };
  stream.end = () => stream.emit("close");
  let opens = 0;
  const plugin = createOpenCodeStreamPlugin({descriptor:"3"},()=>{opens++;return stream;});
  await plugin.event({event:{type:"session.updated",properties:{info:{id:"existing"}}}});
  assert.equal(opens,0);
  await plugin.event({event:{type:"session.created",properties:{info:{id:"fresh"}}}});
  available = false;
  await plugin.event({event:{type:"session.status",properties:{sessionID:"fresh",status:{type:"busy"}}}});
  const count = frames.length;
  available = true;
  await plugin.event({event:{type:"session.created",properties:{info:{id:"another"}}}});
  await plugin.event({event:{type:"session.status",properties:{sessionID:"fresh",status:{type:"busy"}}}});
  assert.equal(frames.length,count);
  assert.equal(opens,1);
  assert.equal(await createOpenCodeStreamPlugin({},()=>assert.fail("opened")).event(null),undefined);
});
