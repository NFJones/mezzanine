//! Installed classic Copilot hooks through genuine ordinary native creators.
//!
//! Isolated bootstrap invokes a fresh product binary; a local fake vendor uses
//! the exact exec/args bytes. No live vendor, credentials or provider are used.

use super::*;

/// Bare isolated bootstrap twice then actual installed exec hooks enroll the
/// ordinary creator, map wait/activity/Stop, and retire exactly one session.
/// Production async framing/native writer attribution runs end to end; no fd3,
/// launcher credential, user config, exporter or paid vendor execution is used.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires freshly built sibling mez binary; installed Copilot helper qualification"]
async fn external_copilot_installed_hooks_admit_and_retire_without_launcher() {
    qualify_installed_hooks(false, false, false).await;
}

/// The actual installed extension entry joins a local SDK-shaped session and
/// writes its fixed attachment/pulse frames directly as a genuine creator child. The
/// production actor must return the original public observer receipt. Existing
/// classic lifecycle/retirement and zero expense remain unchanged; this fixture
/// does not certify vendor discovery, default activation, idle renewal or usage.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_copilot_installed_extension_attaches_to_original_native_creator() {
    qualify_installed_hooks(true, false, false).await;
}

/// A test-owned Node writer uses installed attachment/pulse transport, then sends
/// fixed terminal frames through the actual Unix listener, bounded actor worker
/// and ledger. This qualifies server ingress, not an owned SDK usage projector.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_copilot_terminal_wire_actor_acks_one_canonical_call() {
    qualify_installed_hooks(true, true, false).await;
}

/// The owned revision6 entry observes SDK-shaped subagent usage synchronously
/// before attachment, tolerates initial transport loss and sends bounded fixed
/// gap/terminal frames after native pulse proof. Duplicate events incur one
/// durable unattributed expense; payload mutation/content never changes it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit MEZ_TEST_NODE_BINARY and freshly built sibling mez required"]
async fn external_copilot_installed_usage_projects_early_subagent_metadata_once() {
    qualify_installed_hooks(true, false, true).await;
}

/// Shares the isolated bare bootstrap, exact installed hooks and real native
/// actor. Only the extension case adds a test-owned bundled-SDK-shaped module;
/// neither case rewrites any installed adapter source or runs a provider.
async fn qualify_installed_hooks(extension: bool, terminal: bool, sdk_usage: bool) {
    use crate::host::async_runtime::{
        AsyncRuntimeActorConfig, AsyncRuntimeControlConnectionConfig, AsyncRuntimeSessionActor,
        serve_async_runtime_control_connection_loop,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Some(mut fixture) = fixture("hold").await else {
        panic!("installed Copilot requires native evidence");
    };
    if extension {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fixture.directory, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        std::fs::set_permissions(
            fixture.directory.join("control.sock"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let helper = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("mez");
    let home = fixture.directory.join("home");
    std::fs::create_dir(&home).unwrap();
    let vendor = home.join(".copilot");
    for result in ["installed", "unchanged"] {
        let output = std::process::Command::new(&helper)
            .args(["--json", "bootstrap", "copilot"])
            .env_clear()
            .env("HOME", &home)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["result"], result);
        assert_eq!(report["usage"], "unverified");
        assert_eq!(report["activation"], "owned-default-enabled");
        let settings: serde_json::Value =
            serde_json::from_slice(&std::fs::read(vendor.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings["enabledFeatureFlags"]["EXTENSIONS"], true);
        assert!(settings.get("experimental").is_none());
    }
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(vendor.join("hooks/mezzanine.json")).unwrap())
            .unwrap();
    let extension_command = if extension {
        let node = std::path::PathBuf::from(
            std::env::var_os("MEZ_TEST_NODE_BINARY").expect("explicit offline Node path"),
        );
        assert!(node.is_absolute() && node.is_file());
        let sdk = vendor.join("node_modules/@github/copilot-sdk");
        std::fs::create_dir_all(&sdk).unwrap();
        std::fs::write(
            sdk.join("package.json"),
            br#"{"type":"module","exports":{"./extension":"./extension.mjs"}}"#,
        )
        .unwrap();
        std::fs::write(sdk.join("extension.mjs"),
            "import assert from 'node:assert/strict'; export async function joinSession(config) { assert.deepEqual(config, {}); return {sessionId:'session-a', on:(name, callback)=>{assert(['session.shutdown','assistant.usage'].includes(name)); assert.equal(typeof callback,'function'); return ()=>{};}}; }\n").unwrap();
        if sdk_usage {
            std::fs::write(sdk.join("extension.mjs"), r#"
import assert from 'node:assert/strict';
export async function joinSession(config) {
 assert.deepEqual(config, {});
 return {sessionId:'session-a',on:(name,callback)=>{
  assert(['assistant.usage','session.shutdown'].includes(name));
  if(name==='assistant.usage') {
   const event={type:'assistant.usage',agentId:'subagent-a',data:{
    accounting:{sourceSessionId:'child-source',sequence:1,usageId:'same-call'},
    model:'fixture-model',inputTokens:10,outputTokens:4,cacheReadTokens:3,cost:999,prompt:'PRIVATE'}};
   callback(event);callback(event);
   callback({type:'assistant.usage',data:{inputTokens:999,prompt:'PRIVATE'}});
   event.data.inputTokens=999;
  }
  return ()=>{};
 }};
}
"#).unwrap();
        }
        let script = fixture.directory.join("installed-extension-assert.mjs");
        std::fs::write(&script,
            "import assert from 'node:assert/strict'; import {pathToFileURL} from 'node:url'; const {observer,lifetime} = await import(pathToFileURL(process.argv[2])); assert.equal(await lifetime.ready,true); await new Promise(resolve=>setTimeout(resolve,11000)); lifetime.stop(); assert.equal(observer.external_session_id, 'session-a'); assert(Object.isFrozen(observer)); assert.equal(Object.keys(observer).length, 6); process.stdout.write('{}\\n');\n").unwrap();
        if terminal {
            std::fs::write(&script, r#"
import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
import {dirname, join} from 'node:path';
import {createConnection} from 'node:net';
import {spawnSync} from 'node:child_process';
const root = dirname(process.argv[2]);
const {attachCopilotObserver, pulseCopilotObserver} = await import(pathToFileURL(join(root,'copilot_extension.mjs')));
const {peerHelper} = await import(pathToFileURL(join(root,'peer_helper.mjs')));
const options = {mez:process.env.MEZ,pane:process.env.MEZ_PANE,session:'session-a',peerHelper};
const original = await attachCopilotObserver(options);
assert(original);
const observer_id = 'a'.repeat(64);
assert.equal((await pulseCopilotObserver(options,original,observer_id,1)).observed,true);
const body = JSON.stringify({jsonrpc:'2.0',id:'terminal-wire',method:'agent/external/copilot-terminal',params:{
  pane_id:options.pane,external_session_id:options.session,generation:original.generation,
  observer_witness:original.observer_witness,observer_id,call_id:'b'.repeat(64),model:'fixture-model',
  counters:{input_tokens:10,output_tokens:4,cached_input_tokens:3}
}});
async function send() {
  return await new Promise((resolve,reject)=>{
    const socket = createConnection({path:options.mez.split('\x1f',1)[0].trim()});
    let bytes = Buffer.alloc(0), done = false;
    const timer = setTimeout(()=>finish(Error('bounded terminal fixture timeout')),3000);
    function finish(error,value) {
      if(done)return; done=true; clearTimeout(timer); socket.destroy();
      if(error)reject(error);else resolve(value);
    }
    socket.pause();
    socket.on('error',error=>finish(error));
    socket.on('close',()=>{if(!done)finish(Error('terminal fixture closed'));});
    socket.on('connect',()=>{
      try {
        const peer=spawnSync(peerHelper,['harness-peer'],{stdio:['ignore','pipe','ignore',socket._handle.fd],env:{},timeout:1000});
        assert.equal(peer.status,0);
        assert.equal(peer.stdout.toString(),'{"protocol":"external-peer/1","verified":true}\n');
        socket.write('Content-Length: '+Buffer.byteLength(body)+'\r\nContent-Type: application/vnd.mezzanine.control+json; version=1\r\n\r\n'+body);
        socket.resume();
      }catch(error){finish(error);}
    });
    socket.on('data',chunk=>{
      try {
        assert(bytes.length+chunk.length<=8448);bytes=Buffer.concat([bytes,chunk]);
        const at=bytes.indexOf('\r\n\r\n');if(at<0)return;
        const length=Number(/Content-Length: ([0-9]+)/i.exec(bytes.subarray(0,at).toString())[1]);
        assert(length<=8192);if(bytes.length===at+4+length)finish(undefined,JSON.parse(bytes.subarray(at+4).toString()));
      }catch(error){finish(error);}
    });
  });
}
for(const applied of [true,false]) {
  const reply=await send();assert.equal(reply.result.durable,true);
  assert.equal(reply.result.applied,applied);assert.equal(reply.result.coverage.origin,'unattributed');
}
process.stdout.write('{}\n');
"#).unwrap();
        }
        Some(format!(
            "exec {} {} {}",
            shlex::try_quote(node.to_str().unwrap()).unwrap(),
            shlex::try_quote(script.to_str().unwrap()).unwrap(),
            shlex::try_quote(
                vendor
                    .join("extensions/mezzanine/extension.mjs")
                    .to_str()
                    .unwrap()
            )
            .unwrap(),
        ))
    } else {
        None
    };
    let usage = fixture.directory.join("usage.sqlite");
    fixture
        .service
        .set_token_usage_store(crate::storage::token_usage::TokenUsageStore::new(&usage));
    let service = std::mem::replace(&mut fixture.service, RuntimeServiceFixture::new().build());
    let (handle, actor) =
        AsyncRuntimeSessionActor::new(service, AsyncRuntimeActorConfig::default()).unwrap();
    let task = tokio::spawn(actor.run());
    let mut events = vec![
        ("sessionStart", 1),
        ("sessionStart", 1),
        ("userPromptSubmitted", 2),
        ("notification", 3),
        ("postToolUse", 4),
        ("agentStop", 5),
        ("sessionEnd", 6),
    ];
    if extension {
        events.insert(2, ("extension", 1));
    }
    for (event, time) in events {
        // Only test-owned compiled argv is quoted. exec keeps the production
        // helper a direct child of the genuine fixture creator, not a shell owner.
        let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
        let command = if event == "extension" {
            extension_command.as_ref().unwrap().clone()
        } else {
            let hook = &document["hooks"][event][0];
            format!(
                "exec {} {}",
                quote(hook["exec"].as_str().unwrap()),
                hook["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| quote(v.as_str().unwrap()))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let launch = serde_json::json!({"command":command,"event":{"sessionId":"session-a","timestamp":time,"source":"startup","notification_type":"permission_prompt","stopReason":"end_turn","stop_hook_active":false,"reason":"user_exit","prompt":"PRIVATE","transcriptPath":"PRIVATE","error":{"message":"PRIVATE"},"usage":{"input_tokens":999}}}).to_string();
        fixture.socket.write_all(&[9]).await.unwrap();
        fixture
            .socket
            .write_all(&u32::try_from(launch.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        fixture.socket.write_all(launch.as_bytes()).await.unwrap();
        for connection_index in 0..if event == "extension" {
            if sdk_usage { 7 } else { 4 }
        } else {
            1
        } {
            let (mut socket, _) =
                tokio::time::timeout(Duration::from_secs(15), fixture.listener.accept())
                    .await
                    .unwrap_or_else(|_| panic!("missing {event} connection {connection_index}"))
                    .unwrap();
            if event == "extension" && !terminal && connection_index == 0 {
                // A lost first transport must leave the installed entry able
                // to attach later. This is transport-loss coverage, not proof
                // of real vendor startup-hook/extension delivery ordering.
                socket.shutdown().await.unwrap();
                continue;
            }
            let mut connection = ControlConnectionState::new(true, false);
            serve_async_runtime_control_connection_loop(
                &mut socket,
                &handle,
                &mut connection,
                AsyncRuntimeControlConnectionConfig::new(
                    8192,
                    crate::runtime::current_effective_uid(),
                )
                .unwrap(),
                |served, _| served >= 1,
            )
            .await
            .unwrap();
            assert!(!connection.initialized());
            assert!(connection.caller_client_id().is_none());
            assert!(connection.unix_origin().unwrap().writer_confirmed());
        }
        let mut neutral = [0];
        tokio::time::timeout(
            Duration::from_secs(10),
            fixture.socket.read_exact(&mut neutral),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(neutral, [1]);
    }
    handle.shutdown().await.unwrap();
    fixture.service = task.await.unwrap().service;
    let registry = fixture.service.control.external_agents();
    assert_eq!(registry.bindings.len(), 1);
    let binding = registry.bindings.values().next().unwrap();
    assert_eq!(binding.harness, "copilot");
    assert!(binding.retired);
    let source = binding.enrollment.as_ref().unwrap();
    assert!(source.usage_owner_is_durable);
    if extension {
        assert_eq!(
            source
                .copilot_events
                .extension_snapshot_for_test()
                .unwrap()
                .0,
            if terminal { 1 } else { 2 }
        );
    }
    assert!(source.producer.matches_parent(
        fixture.connection.unix_origin().unwrap().uid(),
        fixture.connection.unix_origin().unwrap().identity
    ));
    let presentation = binding
        .registration
        .as_ref()
        .unwrap()
        .presentation
        .as_ref()
        .unwrap();
    assert_eq!(presentation.sequence, 4);
    assert_eq!(presentation.state, "ready");
    assert!(registry.enrollments.pending.is_empty());
    assert!(fixture.service.external_agent_rows().is_empty());
    let database = rusqlite::Connection::open(usage).unwrap();
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM token_usage_events", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        i64::from(terminal || sdk_usage)
    );
    if terminal || sdk_usage {
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_copilot_calls", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            database
                .query_row("SELECT count(*) FROM external_usage_requests", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0
        );
        assert_eq!(
            database
                .query_row(
                    "SELECT sum(input_tokens) FROM token_usage_events",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            10
        );
        assert_eq!(
            database
                .query_row(
                    "SELECT count(*) FROM token_usage_events WHERE project_id IS NOT NULL",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }
    assert_eq!(
        database
            .query_row("SELECT count(*) FROM external_usage_sources", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
