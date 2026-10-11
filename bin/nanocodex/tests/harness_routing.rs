//! Real CLI journeys; only the external model providers are synthetic.
//! Evidence: output/harness-routing/<journey>-<uuid>/ includes scenario, outcome,
//! provider requests, and CLI output or the raw terminal transcript.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

use std::{
    collections::HashMap,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    pin::Pin,
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Duration,
};

use axum::{
    Json, Router,
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    serve::ListenerExt as _,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use eyre::{Result, eyre};
use futures_util::{SinkExt as _, StreamExt as _};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    net::TcpListener,
    process::Command,
    time::timeout,
};
use tokio_tungstenite::{accept_async, tungstenite::Message};

const CODEX_MODEL: &str = "gpt-6.1-sol";
const CLAUDE_MODEL: &str = "claude-sonnet-5-5";
const LIMIT: Duration = Duration::from_secs(45);

#[derive(Clone, Copy, PartialEq)]
enum Journey {
    Smoke,
    StaleDirect,
    SiblingMessages,
    Mixed,
    MissingChildAuth,
    Subscription,
    SubscriptionRecovery,
    ProjectContext,
    ContextRouting,
    WaitLoop,
}

struct Provider {
    root: &'static str,
    journey: Journey,
    counts: HashMap<String, usize>,
    log: Vec<Value>,
    artifact: PathBuf,
    pauses: usize,
    cancellations: usize,
    connections: Vec<Value>,
}

impl Provider {
    fn connected(&mut self, family: &str) {
        self.connections
            .push(json!({"family":family,"connection":self.connections.len()+1}));
        std::fs::write(
            self.artifact.join("connections.json"),
            serde_json::to_vec_pretty(&self.connections).unwrap(),
        )
        .unwrap();
    }
    fn respond(&mut self, family: &str, label: &str, request: Value) -> Reply {
        let key = if self.journey == Journey::ContextRouting {
            format!("{family}:{label}")
        } else {
            label.into()
        };
        let stage = *self.counts.entry(key.clone()).or_default();
        *self.counts.get_mut(&key).unwrap() += 1;
        // Responses carries schemas in developer messages; Messages uses tools.
        // Later websocket deltas can retain the previously published catalog.
        let catalog = request["tools"].as_array().or_else(|| {
            request["input"]
                .as_array()?
                .iter()
                .rev()
                .find_map(|item| item["tools"].as_array())
        });
        if let Some(catalog) = catalog {
            let names = catalog
                .iter()
                .map(|tool| tool["name"].as_str().unwrap().trim_start_matches('_'))
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                names,
                ["exec", "wait"].into_iter().collect(),
                "{family} model catalog must be Code Mode only"
            );
        } else {
            assert!(
                family == "codex"
                    && (stage > 0 || label == "followup" || label == "sibling-codex-message"),
                "initial model request omitted tools"
            );
        }
        let reply = self.script(label, stage, &request);
        if matches!(reply, Reply::Pause) {
            self.pauses += 1;
        }
        self.log.push(
            json!({"family":family,"label":label,"stage":stage,"model":request["model"],
            "tool_result":last_tool_result(&request),"request":request,"reply":reply.value()}),
        );
        std::fs::write(
            self.artifact.join("provider.json"),
            serde_json::to_vec_pretty(&self.log).unwrap(),
        )
        .unwrap();
        reply
    }

    fn script(&self, label: &str, stage: usize, request: &Value) -> Reply {
        if self.journey == Journey::SiblingMessages {
            let unique = self.artifact.file_name().unwrap().to_str().unwrap();
            let outbound = format!("CLAUDE_TO_CODEX_{unique}");
            let reply_payload = format!("CODEX_TO_CLAUDE_{unique}");
            let metadata = |field: &str| -> u64 {
                user_text(request)
                    .rsplit_once(field)
                    .expect("missing directed-message metadata")
                    .1
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap()
            };
            return match (label, stage) {
                ("root", 0) => Reply::Code(format!(r#"
const c=await tools.spawn_agent({{harness:'codex',model:'{CODEX_MODEL}',role:'codex-peer',task:'SIBLING_CODEX',thinking:null,output_contract:{contract}}});
store('codex-peer',c.agent_id);
const ready=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}}); text(ready);
if(ready.agents[0].status.output.answer!=='codex-ready') throw Error('recipient not ready');
const a=await tools.spawn_agent({{harness:'claude',model:'{CLAUDE_MODEL}',role:'claude-peer',task:'SIBLING_CLAUDE',thinking:null,output_contract:{contract}}});
store('claude-peer',a.agent_id);
text(await tools.wait_agent({{agent_ids:[a.agent_id],timeout_ms:20000}}));
"#, contract=contract())),
                ("root", 1) => Reply::Code(r#"
const c=await tools.wait_agent({agent_ids:[load('codex-peer')],timeout_ms:20000}); text(c);
if(c.agents[0].status.output.answer!=='codex-replied') throw Error('Codex recipient did not reply');
const a=await tools.wait_agent({agent_ids:[load('claude-peer')],timeout_ms:20000}); text(a);
if(a.agents[0].status.output.answer!=='claude-received-reply') throw Error('Claude sender did not receive reply');
text('SIBLING_EXCHANGE_OK');
"#.into()),
                ("root", _) => Reply::Text("sibling-messages-answer".into()),
                ("sibling-codex", 0) => Reply::Code("text(await tools.submit_result({output:{answer:'codex-ready'}}));".into()),
                ("sibling-codex", _) => Reply::Text("Codex recipient ready".into()),
                ("sibling-claude", 0) => Reply::Code(format!(r#"
const peers=await tools.list_agents({{include_completed:true}});
const peer=peers.agents.find(a=>a.role==='codex-peer'); if(!peer) throw Error('missing Codex sibling');
const sent=await tools.send_agent_message({{agent_id:peer.agent_id,purpose:'coordinate',priority:'deferred',message:{payload}}});
store('exchange-thread',sent.thread_id); text({{sent}});
text(await tools.submit_result({{output:{{answer:'claude-sent'}}}}));
"#,payload=json!(outbound))),
                ("sibling-claude", _) => Reply::Text("Claude message sent".into()),
                ("sibling-codex-message", 0) => {
                    assert_eq!(user_text(request).rsplit_once("\nMessage body:\n").unwrap().1.lines().next().unwrap(), outbound, "Codex did not receive exact sibling payload");
                    Reply::Code(format!(r#"
const peers=await tools.list_agents({{include_completed:true}});
const peer=peers.agents.find(a=>a.role==='claude-peer'); if(!peer) throw Error('missing Claude sibling');
const replied=await tools.send_agent_message({{agent_id:peer.agent_id,purpose:'reply',priority:'deferred',in_reply_to:{message_id},message:{payload}}});
if(replied.thread_id!=={thread_id}) throw Error('reply changed thread'); text({{replied}});
text(await tools.submit_result({{output:{{answer:'codex-replied'}}}}));
"#,message_id=metadata("Message ID: "),thread_id=metadata("Thread ID: "),payload=json!(reply_payload)))
                }
                ("sibling-codex-message", _) => Reply::Text("Codex sibling replied".into()),
                ("sibling-claude-reply", 0) => {
                    assert_eq!(user_text(request).rsplit_once("\nMessage body:\n").unwrap().1.lines().next().unwrap(), reply_payload, "Claude did not receive exact sibling reply");
                    Reply::Code(format!(r#"
if(load('exchange-thread')!=={thread_id}) throw Error('received reply on wrong thread');
text(await tools.submit_result({{output:{{answer:'claude-received-reply'}}}}));
"#,thread_id=metadata("Thread ID: ")))
                }
                ("sibling-claude-reply", _) => Reply::Text("Claude received sibling reply".into()),
                _ => panic!("unexpected sibling route {label}/{stage}"),
            };
        }
        if self.journey == Journey::StaleDirect {
            return match stage {
                0 => Reply::DirectExecCommand,
                1 => Reply::Code("text(await tools.exec_command({cmd:\"printf recovered > recovered.txt\",shell:\"/bin/sh\",login:false}));".into()),
                _ => Reply::Text("stale-direct-recovered".into()),
            };
        }
        if self.journey == Journey::ProjectContext {
            return match stage {
                0 => Reply::Read {
                    path: ".agents/skills/fixture/SKILL.md",
                },
                1 => Reply::Write {
                    path: "context-effect.txt",
                    content: "context-effect",
                },
                _ => Reply::Text("project-context-answer".into()),
            };
        }
        if self.journey == Journey::ContextRouting {
            return match (label, stage) {
                ("root", 0) => Reply::Code(format!(
                    r#"
for (const family of ['claude', 'codex']) {{
  const child = await tools.spawn_agent({{harness:family,model:null,role:'MIXED_CHILD',task:'MIXED_CHILD',thinking:null,output_contract:{}}});
  const done = await tools.wait_agent({{agent_ids:[child.agent_id],timeout_ms:20000}});text(done);
  if(done.timed_out || done.agents[0].status.output.answer !== 'context-child-answer') throw Error('context child failed');
}}
text('context-routing-ok');
"#,
                    contract()
                )),
                ("child", 0) => Reply::Code(
                    "text(await tools.submit_result({output:{answer:'context-child-answer'}}));"
                        .into(),
                ),
                ("root", _) => Reply::Text("context-routing-answer".into()),
                _ => Reply::Text("context child finished".into()),
            };
        }

        if self.journey == Journey::SubscriptionRecovery && label == "root" {
            return match stage {
                0 => Reply::Code(format!(
                    r#"
let denied=false;
try {{ await tools.spawn_agent({{harness:'claude',model:'{CLAUDE_MODEL}',role:'MIXED_CHILD',task:'MIXED_CHILD',thinking:null,output_contract:{}}}); }}
catch(e) {{ denied=true; text(String(e)); if(!String(e).includes('nanocodex --claude auth login')) throw Error('missing login hint'); }}
if(!denied) throw Error('signed-out child admitted');
const d=await tools.list_agents({{include_completed:true}}); text(d);
if(d.agents.length) throw Error('failed spawn retained child');
text('subscription-login-required-ok');
"#,
                    contract()
                )),
                1 => Reply::Code(format!(
                    r#"
const c=await tools.spawn_agent({{harness:'claude',model:'{CLAUDE_MODEL}',role:'MIXED_CHILD',task:'MIXED_CHILD',thinking:null,output_contract:{}}});
const w=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}}); text(w);
if(w.timed_out || w.agents[0].status.state!=='completed' || w.agents[0].status.output.answer!=='subscription-child-answer') throw Error('child failed after login');
const d=await tools.list_agents({{include_completed:true}}); text(d);
if(d.agents.length!==1) throw Error('recovery retained an extra child');
text('subscription-same-process-recovered-ok');
"#,
                    contract()
                )),
                _ => Reply::Text("subscription-recovery-answer".into()),
            };
        }
        if matches!(
            self.journey,
            Journey::Subscription | Journey::SubscriptionRecovery
        ) {
            return match (self.root, label, stage) {
                ("claude", "root", 0) => Reply::Write {
                    path: "grandchild.txt",
                    content: "grandchild-effect",
                },
                ("claude", "root", _) => Reply::Text("subscription-native-answer".into()),
                ("codex", "root", 0) => Reply::Code(format!(r#"
const c=await tools.spawn_agent({{harness:'claude',model:'{CLAUDE_MODEL}',role:'MIXED_CHILD',task:'MIXED_CHILD',thinking:null,output_contract:{}}});
const w=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}}); text(w);
if(w.timed_out || w.agents[0].status.state!=='completed' || w.agents[0].status.output.answer!=='subscription-child-answer') throw Error('subscription child failed');
text('subscription-child-ok');
"#, contract())),
                ("codex", "root", _) => Reply::Text("subscription-mixed-answer".into()),
                ("codex", "child", 0) => Reply::Write {
                    path: "subscription-child.txt",
                    content: "subscription-child-effect",
                },
                ("codex", "child", 1) => Reply::Code("const receipt=await tools.submit_result({output:{answer:'subscription-child-answer'}}); text(receipt); if(!receipt.accepted) throw Error('subscription result rejected');".into()),
                ("codex", "child", _) => Reply::Text("subscription child finished".into()),
                _ => Reply::Text("unexpected subscription request".into()),
            };
        }
        if self.journey == Journey::Smoke {
            return Reply::Text("claude-only-answer".into());
        }
        if self.journey == Journey::WaitLoop {
            // A parent that re-waits on the full ID list must block on the
            // still-running child instead of re-reading the finished one.
            let spawn = |task: &str| {
                json!({"role":task,"task":task,"output_contract":contract(),"thinking":null})
                    .to_string()
            };
            return match (label, stage) {
                ("root", 0) => Reply::Code(format!(r#"
const fast=await tools.spawn_agent({}); const slow=await tools.spawn_agent({});
const ids=[fast.agent_id, slow.agent_id]; const seen=new Set(); let calls=0, timeouts=0;
const t0=Date.now();
while(Date.now()-t0<1500) {{
  const w=await tools.wait_agent({{agent_ids:ids,timeout_ms:250}}); calls++;
  if(w.timed_out) timeouts++;
  for(const a of w.agents) if(a.status.state==='completed') seen.add(a.agent_id);
}}
if(!seen.has(fast.agent_id)) throw Error('fast result never reported');
if(calls>12 || timeouts<calls-1) throw Error('full-list wait did not block: calls='+calls+' timeouts='+timeouts);
await tools.close_agent({{agent_id:slow.agent_id}});
const closed=await tools.wait_agent({{agent_ids:ids,timeout_ms:250}});
if(closed.timed_out || closed.agents[1].status.state!=='closed') throw Error('close was not reported');
const r0=Date.now(); const reread=await tools.wait_agent({{agent_ids:ids,timeout_ms:20000}});
if(reread.timed_out || Date.now()-r0>2000) throw Error('re-read of terminal agents blocked');
let stuck='';
try {{ await tools.wait_agent({{agent_ids:ids,timeout_ms:20000}}); await tools.wait_agent({{agent_ids:ids,timeout_ms:20000}}); }} catch(e) {{ stuck=String(e.message||e); }}
if(!stuck.includes('nothing is left to wait for')) throw Error('stuck wait loop was not rejected: '+stuck);
text({{calls,timeouts,stuck}}); text('wait-loop-ok');
"#, spawn("WAIT_FAST_CHILD"), spawn("WAIT_SLOW_CHILD"))),
                ("root", _) => Reply::Text(
                    if last_tool_result(request).to_string().contains("wait-loop-ok") {
                        "wait-loop-answer"
                    } else {
                        "wait-loop-failed"
                    }
                    .into(),
                ),
                ("wait-fast", 0) => Reply::Code("const r=await tools.submit_result({output:{answer:'fast'}}); if(!r.accepted) throw Error('fast rejected');".into()),
                ("wait-fast", _) => Reply::Text("fast child finished".into()),
                ("wait-slow", _) => Reply::Pause,
                _ => Reply::Text("unexpected wait-loop request".into()),
            };
        }
        let other = if self.root == "codex" {
            "claude"
        } else {
            "codex"
        };
        let spawn = |family: Option<&str>, task: &str| {
            let mut value =
                json!({"role":task,"task":task,"output_contract":contract(),"thinking":null});
            if let Some(family) = family {
                value["harness"] = json!(family);
                value["model"] = json!(model(family));
            }
            value.to_string()
        };
        if self.journey == Journey::MissingChildAuth {
            return if stage == 0 {
                Reply::Code(format!(
                    "let denied=false; try {{ await tools.spawn_agent({}); }} catch(e) {{ denied=true; text(String(e)); {} }} if(!denied) throw Error('missing credentials admitted'); const d=await tools.list_agents({{include_completed:true}}); if(d.agents.length) throw Error('failed spawn retained child'); text('auth-denied-ok');",
                    spawn(Some(other), "AUTH_CHILD"),
                    if other == "claude" {
                        "if(!String(e).includes('nanocodex --claude auth login')) throw Error('missing login hint');"
                    } else {
                        ""
                    }
                ))
            } else {
                Reply::Text("auth-denied-answer".into())
            };
        }
        match (label, stage) {
            ("root", 0) => Reply::Code(format!(r#"
const base={}; let rejected=0;
for(const override of [{{harness:'bogus'}},{{harness:'claude',model:'sol'}},{{harness:'codex',model:'claude-sonnet-4-6'}}]) {{
  try {{ await tools.spawn_agent({{...base,...override}}); }} catch(e) {{ rejected++; text(String(e)); }}
}}
if(rejected!==3) throw Error('invalid selection admitted');
const empty=await tools.list_agents({{include_completed:true}}); if(empty.agents.length) throw Error('invalid selection had registry effects');
const c=await tools.spawn_agent({}); store('child',c.agent_id);
const w=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}}); text(w);
if(w.timed_out || w.agents[0].status.state!=='completed' || w.agents[0].status.output.answer!=='nested-answer') throw Error('mixed child failed');
const inherited=await tools.spawn_agent({});
const same=await tools.wait_agent({{agent_ids:[inherited.agent_id],timeout_ms:20000}}); text(same);
if(same.timed_out || same.agents[0].status.output.answer!=='inherited-answer') throw Error('inheritance failed');
text('nested-and-inherited-ok');
"#, spawn(None,"INVALID_CHILD"), spawn(Some(other),"MIXED_CHILD"), spawn(None,"INHERITED_CHILD"))),
            ("root", 1) => Reply::Code(r#"
const id=load('child'); const receipt=await tools.send_agent_message({agent_id:id,message:'FOLLOWUP_CHILD'}); text(receipt);
const w=await tools.wait_agent({agent_ids:[id],timeout_ms:20000}); text(w);
if(w.timed_out || w.agents[0].status.state!=='completed' || w.agents[0].status.output.answer!=='followup-answer') throw Error('reuse failed');
text('reuse-ok');
"#.into()),
            ("root", 2) => Reply::Code(r#"
const c=await tools.close_agent({agent_id:load('child')}); text(c);
if(c.agents.length!==2 || c.agents.some(a=>a.status.state!=='closed')) throw Error('subtree not closed');
let denied=false; try { await tools.send_agent_message({agent_id:load('child'),message:'cannot revive'}); } catch(e) { denied=true; text(String(e)); }
if(!denied) throw Error('closed child reused');
text(await tools.list_agents({include_completed:true})); text('closed-subtree-ok');
"#.into()),
            ("root", 3) => Reply::Code(format!(r#"
const c=await tools.spawn_agent({});
const waiting=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:1000}}); text(waiting);
if(!waiting.timed_out || waiting.agents[0].status.state!=='running') throw Error('paused child did not remain active');
const interrupted=await tools.interrupt_agent({{agent_id:c.agent_id}}); text(interrupted);
if(interrupted.agents.length!==2 || interrupted.agents.some(a=>a.status.state!=='interrupted')) throw Error('mixed subtree not interrupted');
const closed=await tools.close_agent({{agent_id:c.agent_id}}); text(closed);
if(closed.agents.length!==2 || closed.agents.some(a=>a.status.state!=='closed')) throw Error('interrupted subtree not closed');
text('paused-subtree-released-ok');
"#,spawn(Some(other),"PAUSED_CHILD"))),
            ("root", _) => Reply::Text("mixed-routing-answer".into()),
            ("paused-child", 0) => Reply::Code(format!("const c=await tools.spawn_agent({}); text(await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}}));",spawn(Some(self.root),"PAUSED_GRANDCHILD"))),
            ("paused-grandchild", _) => Reply::Pause,
            ("child", 0) => Reply::Code(format!(r#"
const c=await tools.spawn_agent({}); const w=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}}); text(w);
if(w.timed_out || w.agents[0].status.state!=='completed' || w.agents[0].status.output.answer!=='grandchild-answer') throw Error('grandchild failed');
text('grandchild-ok');
"#,spawn(Some(self.root),"MIXED_GRANDCHILD"))),
            ("child", 1) => Reply::Code(r#"
let denied=false; try { await tools.submit_result({output:{answer:42}}); } catch(e) { denied=true; text(String(e)); }
if(!denied) throw Error('invalid output contract admitted');
const receipt=await tools.submit_result({output:{answer:'nested-answer'}}); text(receipt);
if(!receipt.accepted) throw Error('valid output rejected'); text('contract-recovered-ok');
"#.into()),
            ("child", _) => Reply::Text("child finished".into()),
            ("followup", 0) => Reply::Code("text(await tools.submit_result({output:{answer:'followup-answer'}}));".into()),
            ("followup", _) => Reply::Text("followup finished".into()),
            ("grandchild", 0) if self.root == "claude" => Reply::Write {
                path: "grandchild.txt",
                content: "grandchild-effect",
            },
            ("grandchild", 1) if self.root == "claude" => Reply::Code("text(await tools.submit_result({output:{answer:'grandchild-answer'}}));".into()),
            ("grandchild", 0) => Reply::Code(r#"
text(await tools.exec_command({cmd:"printf 'grandchild-effect' > grandchild.txt",shell:'/bin/sh',login:false}));
text(await tools.submit_result({output:{answer:'grandchild-answer'}}));
"#.into()),
            ("grandchild", _) => Reply::Text("grandchild finished".into()),
            ("inherited", 0) => Reply::Code("text(await tools.submit_result({output:{answer:'inherited-answer'}}));".into()),
            ("inherited", _) => Reply::Text("inherited finished".into()),
            _ => Reply::Text("unexpected fixture request".into()),
        }
    }
}

enum Reply {
    DirectExecCommand,
    Read {
        path: &'static str,
    },
    Code(String),
    Text(String),
    Write {
        path: &'static str,
        content: &'static str,
    },
    Pause,
}
impl Reply {
    fn value(&self) -> Value {
        match self {
            Self::DirectExecCommand => {
                json!({"tool":"exec_command","cmd":"printf bypass > stale-direct.txt"})
            }
            Self::Read { path } => json!({"tool":"Read","file_path":path}),
            Self::Code(code) => json!({"code":code}),
            Self::Text(text) => json!({"text":text}),
            Self::Write { path, content } => {
                json!({"tool":"Write","file_path":path,"content":content})
            }
            Self::Pause => json!({"paused":true}),
        }
    }
    fn codex(&self, id: &str) -> Value {
        let output = match self {
            Self::DirectExecCommand => {
                json!({"type":"function_call","name":"exec_command","call_id":id,"arguments":json!({"cmd":"printf bypass > stale-direct.txt","shell":"/bin/sh","login":false}).to_string()})
            }
            Self::Code(code) => {
                json!({"type":"custom_tool_call","name":"exec","call_id":id,"input":code})
            }
            Self::Text(text) => {
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
            }
            Self::Read { .. } | Self::Write { .. } => {
                unreachable!("native file tools cannot route to Codex Responses")
            }
            Self::Pause => unreachable!("paused generation has no terminal response"),
        };
        json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[output],
            "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}}})
    }
    fn claude(&self, id: &str, request: &Value) -> String {
        let tool_name = |name: &str| {
            if request["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == format!("_{name}")))
            {
                format!("_{name}")
            } else {
                name.to_owned()
            }
        };
        let (block, delta, stop) = match self {
            Self::DirectExecCommand => unreachable!("Codex stale direct call fixture"),
            Self::Read { path } => code_block(
                &format!(
                    "text(await nativeTools.Read({{file_path:{}}}));",
                    json!(path)
                ),
                id,
                &tool_name("exec"),
            ),
            Self::Code(code) => code_block(code, id, &tool_name("exec")),
            Self::Text(text) => (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":text}),
                "end_turn",
            ),
            Self::Write { path, content } => code_block(
                &format!(
                    "text(await nativeTools.Write({{file_path:{},content:{}}}));",
                    json!(path),
                    json!(content)
                ),
                id,
                &tool_name("exec"),
            ),
            Self::Pause => unreachable!("paused generation has no terminal response"),
        };
        [json!({"type":"message_start","message":{"id":id,"type":"message","role":"assistant","model":CLAUDE_MODEL,"content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
            json!({"type":"content_block_start","index":0,"content_block":block}),
            json!({"type":"content_block_delta","index":0,"delta":delta}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":1}}),
            json!({"type":"message_stop"})].iter().map(|event|format!("data: {event}\n\n")).collect()
    }
}

fn code_block(code: &str, id: &str, name: &str) -> (Value, Value, &'static str) {
    let code = format!("const nativeTools = tools;\n{code}");
    (
        json!({"type":"tool_use","id":id,"name":name,"input":{}}),
        json!({"type":"input_json_delta","partial_json":json!({"code":code}).to_string()}),
        "tool_use",
    )
}

fn contract() -> Value {
    json!({"kind":"object","fields":[{"name":"answer","required":true,"schema":{"kind":"string"}}]})
}
fn model(family: &str) -> &'static str {
    if family == "claude" {
        CLAUDE_MODEL
    } else {
        CODEX_MODEL
    }
}

// Routing uses user input only, never a tool catalog or fixture-script copy.
fn user_text(request: &Value) -> String {
    request
        .get("messages")
        .or_else(|| request.get("input"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "user")
        .flat_map(|item| match &item["content"] {
            Value::String(text) => vec![text.clone()],
            Value::Array(blocks) => blocks
                .iter()
                .filter(|block| block["type"] == "text" || block["type"] == "input_text")
                .filter_map(|block| block["text"].as_str().map(str::to_owned))
                .collect(),
            _ => vec![],
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn label(request: &Value) -> String {
    let input = user_text(request);
    if input.contains("CODEX_TO_CLAUDE_") {
        "sibling-claude-reply"
    } else if input.contains("CLAUDE_TO_CODEX_") {
        "sibling-codex-message"
    } else if input.contains("SIBLING_CODEX") {
        "sibling-codex"
    } else if input.contains("SIBLING_CLAUDE") {
        "sibling-claude"
    } else if input.contains("PAUSED_GRANDCHILD") {
        "paused-grandchild"
    } else if input.contains("PAUSED_CHILD") {
        "paused-child"
    } else if input.contains("FOLLOWUP_CHILD") {
        "followup"
    } else if input.contains("MIXED_GRANDCHILD") {
        "grandchild"
    } else if input.contains("INHERITED_CHILD") {
        "inherited"
    } else if input.contains("MIXED_CHILD") {
        "child"
    } else if input.contains("WAIT_FAST_CHILD") {
        "wait-fast"
    } else if input.contains("WAIT_SLOW_CHILD") {
        "wait-slow"
    } else {
        "root"
    }
    .into()
}

fn last_tool_result(request: &Value) -> Value {
    if let Some(messages) = request["messages"].as_array() {
        return messages
            .iter()
            .rev()
            .filter_map(|m| m["content"].as_array())
            .flat_map(|blocks| blocks.iter().rev())
            .find(|b| b["type"] == "tool_result")
            .map(|b| b["content"].clone())
            .unwrap_or(Value::Null);
    }
    request["input"]
        .as_array()
        .and_then(|items| {
            items.iter().rev().find(|item| {
                item["type"] == "custom_tool_call_output" || item["type"] == "function_call_output"
            })
        })
        .map(|item| item["output"].clone())
        .unwrap_or(Value::Null)
}

struct Servers {
    codex: String,
    claude: String,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

// Secrets remain in fixture memory; persisted HTTP evidence is redacted.
struct SubscriptionFixture {
    artifact: PathBuf,
    oauth: Vec<Value>,
    messages: Vec<Value>,
    bodies: Vec<Vec<u8>>,
    login_query: HashMap<String, String>,
    tokens: usize,
    reject_next: bool,
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(
                    name.as_str(),
                    "authorization"
                        | "access_token"
                        | "refresh_token"
                        | "token"
                        | "code"
                        | "code_verifier"
                        | "state"
                ) {
                    *value = json!("<redacted>");
                } else {
                    redact(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact),
        _ => {}
    }
}

impl SubscriptionFixture {
    fn record(&mut self, mut event: Value) {
        redact(&mut event);
        self.oauth.push(event);
        std::fs::write(
            self.artifact.join("oauth-http.json"),
            serde_json::to_vec_pretty(&self.oauth).unwrap(),
        )
        .unwrap();
    }

    fn token(&mut self, headers: &HeaderMap, body: Value) -> Value {
        assert!(!headers.contains_key("anthropic-beta"));
        assert_eq!(body["client_id"], "synthetic-cli-client");
        if self.tokens == 0 {
            assert_eq!(body["grant_type"], "authorization_code");
            assert_eq!(body["code"], "synthetic-login-code");
            assert_eq!(body["state"], self.login_query["state"]);
            let verifier = body["code_verifier"].as_str().unwrap();
            assert_eq!(
                URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes())),
                self.login_query["code_challenge"]
            );
            assert_eq!(self.login_query["code_challenge_method"], "S256");
        } else {
            assert_eq!(self.tokens, 1, "unexpected refresh or second login");
            assert_eq!(body["grant_type"], "refresh_token");
            assert_eq!(body["refresh_token"], "synthetic-refresh-1");
        }
        self.tokens += 1;
        self.record(json!({"endpoint":"token","request":body,"status":200}));
        json!({"access_token":format!("synthetic-access-{}",self.tokens),"refresh_token":format!("synthetic-refresh-{}",self.tokens),"expires_in":3600,"scope":"user:profile user:inference user:sessions:claude_code","token_type":"Bearer"})
    }

    fn profile(&mut self, headers: &HeaderMap) {
        assert_eq!(
            headers["authorization"].to_str().unwrap(),
            format!("Bearer synthetic-access-{}", self.tokens)
        );
        assert!(!headers.contains_key("anthropic-beta"));
        self.record(json!({"endpoint":"profile","authorization":"<redacted>","status":200}));
    }

    fn messages(&mut self, headers: &HeaderMap, body: &[u8], request: &Value) -> bool {
        assert_eq!(
            headers["authorization"].to_str().unwrap(),
            format!("Bearer synthetic-access-{}", self.tokens)
        );
        assert!(
            !headers.contains_key("x-api-key"),
            "subscription accidentally used an API key"
        );
        assert!(
            headers["anthropic-beta"]
                .to_str()
                .unwrap()
                .split(',')
                .any(|beta| beta.trim() == "oauth-2025-04-20")
        );
        assert_eq!(headers["x-app"], "cli");
        assert_eq!(headers["x-stainless-runtime"], "node");
        let user_agent = headers["user-agent"].to_str().unwrap();
        assert!(user_agent.starts_with("claude-cli/"));
        let accepted = !std::mem::take(&mut self.reject_next);
        self.bodies.push(body.to_vec());
        self.messages.push(json!({"endpoint":"messages","authorization":"<redacted>","token_generation":self.tokens,"anthropic-beta":headers["anthropic-beta"].to_str().unwrap(),"user-agent":user_agent,"x-app":"cli","x-stainless-runtime":"node","status":if accepted {200} else {401},"request":request}));
        std::fs::write(
            self.artifact.join("messages-http.json"),
            serde_json::to_vec_pretty(&self.messages).unwrap(),
        )
        .unwrap();
        accepted
    }
}

struct PauseGuard {
    state: Arc<Mutex<Provider>>,
    observed: bool,
}
impl Drop for PauseGuard {
    fn drop(&mut self) {
        if !self.observed {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.cancellations += 1;
        std::fs::write(
            state.artifact.join("paused-provider.json"),
            serde_json::to_vec_pretty(
                &json!({"started":state.pauses,"released":state.cancellations}),
            )
            .unwrap(),
        )
        .unwrap();
    }
}
struct PausedStream {
    _guard: PauseGuard,
}
impl futures_util::Stream for PausedStream {
    type Item = std::result::Result<axum::body::Bytes, std::convert::Infallible>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Pending
    }
}
impl Drop for Servers {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn servers(provider: Arc<Mutex<Provider>>) -> Result<Servers> {
    subscription_servers(provider, None).await
}

async fn subscription_servers(
    provider: Arc<Mutex<Provider>>,
    subscription: Option<Arc<Mutex<SubscriptionFixture>>>,
) -> Result<Servers> {
    subscription_servers_with_recovery(provider, subscription, None).await
}

#[derive(Default)]
struct SubscriptionRecoveryGate {
    denied: tokio::sync::Notify,
    logged_in: tokio::sync::Notify,
}

async fn subscription_servers_with_recovery(
    provider: Arc<Mutex<Provider>>,
    subscription: Option<Arc<Mutex<SubscriptionFixture>>>,
    recovery: Option<Arc<SubscriptionRecoveryGate>>,
) -> Result<Servers> {
    let http = TcpListener::bind("127.0.0.1:0").await?;
    let claude = format!("http://{}/v1/messages", http.local_addr()?);
    let state = Arc::clone(&provider);
    let messages_auth = subscription.clone();
    let mut router = Router::new().route(
        "/v1/messages",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let state = Arc::clone(&state);
            let auth = messages_auth.clone();
            async move {
                let request: Value = serde_json::from_slice(&body).unwrap();
                if let Some(auth) = auth
                    && !auth.lock().unwrap().messages(&headers, &body, &request)
                {
                    return (axum::http::StatusCode::UNAUTHORIZED, Json(json!({"type":"error","error":{"type":"authentication_error","message":"synthetic access expired"}}))).into_response();
                }
                let label = label(&request);
                let reply = state.lock().unwrap().respond("claude", &label, request.clone());
                if matches!(reply, Reply::Pause) {
                    return axum::response::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(axum::body::Body::from_stream(PausedStream {
                            _guard: PauseGuard {
                                state,
                                observed: true,
                            },
                        }))
                        .unwrap();
                }
                (
                    [("content-type", "text/event-stream")],
                    reply.claude(&uuid::Uuid::new_v4().to_string(), &request),
                )
                    .into_response()
            }
        }),
    );
    if let Some(auth) = subscription {
        let token_auth = Arc::clone(&auth);
        let profile_auth = Arc::clone(&auth);
        router = router
            .route("/oauth/token", post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let response = token_auth.lock().unwrap().token(&headers, body);
                async move { Json(response) }
            }))
            .route("/oauth/profile", get(move |headers: HeaderMap| {
                let mut state = profile_auth.lock().unwrap();
                state.profile(&headers);
                async { Json(json!({"account":{"uuid":"synthetic-account"},"organization":{"uuid":"synthetic-org"}})) }
            }))
            .route("/oauth/token/revoke", post(move |Json(body): Json<Value>| {
                let mut state = auth.lock().unwrap();
                assert_eq!(body["token"], "synthetic-refresh-2");
                state.record(json!({"endpoint":"revoke","request":body,"status":200}));
                async { Json(json!({})) }
            }));
    }
    let connections = Arc::clone(&provider);
    let http = http.tap_io(move |_| connections.lock().unwrap().connected("claude"));
    let http_task = tokio::spawn(async move {
        axum::serve(http, router).await.unwrap();
    });
    let ws = TcpListener::bind("127.0.0.1:0").await?;
    let codex = format!("ws://{}", ws.local_addr()?);
    let ws_task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = ws.accept().await.unwrap();
            provider.lock().unwrap().connected("codex");
            let state = Arc::clone(&provider);
            let recovery = recovery.clone();
            connections.spawn(async move {
                let Ok(mut socket) = accept_async(stream).await else {
                    return;
                };
                let mut session_label = None;
                while let Some(Ok(message)) = socket.next().await {
                    let Message::Text(text) = message else {
                        continue;
                    };
                    let request: Value = serde_json::from_str(text.as_str()).unwrap();
                    let current = label(&request);
                    if session_label.is_none() || current == "followup" || current == "sibling-codex-message" {
                        session_label = Some(current);
                    }
                    // Pause only the external provider response, after observing
                    // the real CLI's failed-spawn tool result. The owning Codex
                    // runtime remains alive while a second CLI process logs in.
                    if let Some(recovery) = &recovery {
                        let denied = {
                            let state = state.lock().unwrap();
                            state.counts.get("root") == Some(&1)
                                && session_label.as_deref() == Some("root")
                        };
                        if denied {
                            let artifact = state.lock().unwrap().artifact.clone();
                            std::fs::write(
                                artifact.join("signed-out-boundary.json"),
                                serde_json::to_vec_pretty(&json!({
                                    "observed_tool_result":last_tool_result(&request),
                                    "next_step":"second-process shipped CLI OAuth login, then retry from this live Codex runtime"
                                })).unwrap(),
                            ).unwrap();
                            recovery.denied.notify_one();
                            recovery.logged_in.notified().await;
                        }
                    }
                    let reply = state.lock().unwrap().respond(
                        "codex",
                        session_label.as_deref().unwrap(),
                        request,
                    );
                    if matches!(reply, Reply::Pause) {
                        let mut guard = PauseGuard {
                            state: Arc::clone(&state),
                            observed: false,
                        };
                        guard.observed = timeout(Duration::from_secs(20), async {
                            while let Some(Ok(message)) = socket.next().await {
                                if matches!(message, Message::Close(_)) {
                                    break;
                                }
                            }
                        })
                        .await
                        .is_ok();
                        break;
                    }
                    if socket
                        .send(Message::Text(
                            reply
                                .codex(&uuid::Uuid::new_v4().to_string())
                                .to_string()
                                .into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    Ok(Servers {
        codex,
        claude,
        tasks: vec![http_task, ws_task],
    })
}

fn command(
    workspace: &Path,
    endpoints: &Servers,
    family: &str,
    with_codex_auth: bool,
    with_claude_auth: bool,
    root_prompt: bool,
) -> Command {
    let mut command = Command::new(local_cli());
    if !root_prompt {
        command.arg("run");
    }
    command
        .current_dir(workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CODEX_HOME", workspace.join("codex-home"))
        .env("NANOCODEX_COMPUTER", "off")
        .args([
            "--thinking",
            "medium",
            "--websocket-url",
            &endpoints.codex,
            "--claude-messages-url",
            &endpoints.claude,
            "--websocket-warmup",
            "false",
            "--responses-transport",
            "websocket",
            "--store-responses",
            "false",
            "--rollouts",
            "false",
            "--browser=none",
            "--mcp-defaults",
            "false",
            "--mcp-codex-config",
            "false",
            "--web-search",
            "false",
            "--image-generation",
            "false",
            "--subagents",
            "true",
            "--memory",
            "false",
        ])
        .arg("--api-base-url")
        .arg(endpoints.claude.trim_end_matches("/messages"))
        .arg("--cwd")
        .arg(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if family == "claude" {
        command.arg("--claude");
    }
    if with_codex_auth {
        command.args(["--api-key", "synthetic-openai-key"]);
    }
    if with_claude_auth {
        command.args(["--claude-api-key", "synthetic-anthropic-key"]);
    }
    command
}

fn artifact(name: &str) -> Result<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/harness-routing")
        .join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(path.join("workspace/home"))?;
    std::fs::write(path.join("provider.json"), "[]\n")?;
    std::fs::write(path.join("connections.json"), "[]\n")?;
    Ok(path)
}

async fn run(mut command: Command, artifact: &Path, expected: &str) -> Result<Output> {
    std::fs::write(
        artifact.join("scenario.json"),
        serde_json::to_vec_pretty(&json!({
        "reproduce":"cargo test --locked -p nanocodex-bin --test harness_routing -- --nocapture",
        "command":format!("{command:?}"),"expected":expected,
        "boundary":"shipped nanocodex executable, native Code Mode/tools/subagent registry; only external providers use loopback fixtures"}))?,
    )?;
    let output = timeout(LIMIT, command.output())
        .await
        .map_err(|_| eyre!("CLI timeout; inspect {}", artifact.display()))??;
    std::fs::write(artifact.join("stdout.jsonl"), &output.stdout)?;
    std::fs::write(artifact.join("stderr.log"), &output.stderr)?;
    std::fs::write(
        artifact.join("outcome.json"),
        serde_json::to_vec_pretty(&json!({
            "exit_code":output.status.code(),"success":output.status.success(),
            "stdout":"stdout.jsonl","stderr":"stderr.log","provider_transcript":"provider.json"
        }))?,
    )?;
    eprintln!("journey evidence: {}", artifact.display());
    Ok(output)
}

struct TuiChild(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for TuiChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

async fn run_tui(command: Command, artifact: &Path, answer: &str) -> Result<()> {
    let command = command.as_std();
    let mut terminal_command = CommandBuilder::new(command.get_program());
    terminal_command.args(command.get_args());
    terminal_command.env_clear();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            terminal_command.env(key, value);
        }
    }
    terminal_command.env("TERM", "xterm-256color");
    if let Some(directory) = command.get_current_dir() {
        terminal_command.cwd(directory);
    }
    std::fs::write(
        artifact.join("scenario.json"),
        serde_json::to_vec_pretty(&json!({
            "reproduce":"cargo test --locked -p nanocodex-bin --test harness_routing claude_root_needs_only_anthropic_credentials -- --nocapture",
            "command":format!("{terminal_command:?}"),
            "expected":format!("TUI displays {answer}, then exits successfully after Ctrl+C Ctrl+C"),
            "boundary":"shipped nanocodex executable in a 140x32 PTY; only external providers use loopback fixtures",
            "input_after_answer":"Ctrl+C Ctrl+C (0x03 0x03)",
            "terminal_transcript":"terminal.log"
        }))?,
    )?;
    let mut transcript = std::fs::File::create(artifact.join("terminal.log"))?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| eyre!(error.to_string()))?;
    let mut child = TuiChild(
        pair.slave
            .spawn_command(terminal_command)
            .map_err(|error| eyre!(error.to_string()))?,
    );
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| eyre!(error.to_string()))?;
    let mut keyboard = pair
        .master
        .take_writer()
        .map_err(|error| eyre!(error.to_string()))?;
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let capture = std::thread::spawn(move || -> std::io::Result<()> {
        let mut bytes = [0; 8192];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            transcript.write_all(&bytes[..count])?;
            let _ = send.send(bytes[..count].to_vec());
        }
        Ok(())
    });
    let mut answer_visible = false;
    let result = timeout(LIMIT, async {
        let mut bytes = Vec::new();
        while !String::from_utf8_lossy(&bytes).contains(answer) {
            let chunk = receive
                .recv()
                .await
                .ok_or_else(|| eyre!("TUI closed before displaying {answer}"))?;
            bytes.extend(chunk);
        }
        answer_visible = true;
        // The unified TUI quits on a confirmed Ctrl+C; Ctrl+D edits the composer.
        keyboard.write_all(b"\x03\x03")?;
        keyboard.flush()?;
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok::<_, eyre::Report>(status);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| eyre!("TUI timed out waiting for its answer or clean exit"))
    .and_then(|result| result);
    drop(child);
    drop(keyboard);
    drop(pair.master);
    let capture_result = tokio::task::spawn_blocking(move || capture.join()).await?;
    std::fs::write(
        artifact.join("outcome.json"),
        serde_json::to_vec_pretty(&json!({
            "exit_code":result.as_ref().ok().map(|status| status.exit_code()),
            "success":result.as_ref().is_ok_and(|status| status.success()),
            "answer_visible":answer_visible,
            "error":result.as_ref().err().map(ToString::to_string),
            "terminal_transcript":"terminal.log","provider_transcript":"provider.json"
        }))?,
    )?;
    eprintln!("journey evidence: {}", artifact.display());
    capture_result.map_err(|_| {
        eyre!(
            "terminal capture thread panicked; evidence {}",
            artifact.display()
        )
    })??;
    let status = result.map_err(|error| eyre!("{error}; evidence {}", artifact.display()))?;
    if !status.success() {
        return Err(eyre!(
            "TUI failed ({status}); evidence {}",
            artifact.display()
        ));
    }
    Ok(())
}

fn success(output: &Output, artifact: &Path, answer: &str) -> Result<()> {
    if !output.status.success() {
        return Err(eyre!(
            "CLI failed; evidence {}\n{}\n{}",
            artifact.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert!(
        events.iter().any(|e| e["type"] == "run.completed"),
        "missing terminal event: {events:?}"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(answer),
        "missing final answer; evidence {}",
        artifact.display()
    );
    Ok(())
}

async fn journey(family: &'static str, kind: Journey) -> Result<()> {
    let artifact = artifact(&format!(
        "{family}-{}",
        if kind == Journey::Mixed {
            "mixed"
        } else if kind == Journey::Smoke {
            "only"
        } else if kind == Journey::SiblingMessages {
            "sibling-messages"
        } else if kind == Journey::WaitLoop {
            "wait-loop"
        } else {
            "missing-auth"
        }
    ))?;
    let provider = Arc::new(Mutex::new(Provider {
        root: family,
        journey: kind,
        counts: HashMap::new(),
        log: vec![],
        artifact: artifact.clone(),
        pauses: 0,
        cancellations: 0,
        connections: vec![],
    }));
    let servers = servers(Arc::clone(&provider)).await?;
    let mut command = command(
        &artifact.join("workspace"),
        &servers,
        family,
        kind == Journey::Mixed || family == "codex",
        matches!(kind, Journey::Mixed | Journey::SiblingMessages) || family == "claude",
        kind == Journey::Smoke,
    );
    command.args(["--model", model(family)]);
    if kind == Journey::Smoke {
        command.arg("--prompt");
    }
    command.arg("HARNESS_ROUTING_ROOT");
    let answer = match kind {
        Journey::Smoke => "claude-only-answer",
        Journey::StaleDirect => "stale-direct-recovered",
        Journey::SiblingMessages => "sibling-messages-answer",
        Journey::Mixed => "mixed-routing-answer",
        Journey::MissingChildAuth => "auth-denied-answer",
        Journey::Subscription => "subscription-native-answer",
        Journey::SubscriptionRecovery => "subscription-recovery-answer",
        Journey::ProjectContext => "project-context-answer",
        Journey::ContextRouting => "context-routing-answer",
        Journey::WaitLoop => "wait-loop-answer",
    };
    if kind == Journey::Smoke {
        run_tui(command, &artifact, answer).await?;
    } else {
        let output = run(command, &artifact, answer).await?;
        success(&output, &artifact, answer)?;
    }
    if kind == Journey::Mixed {
        timeout(Duration::from_secs(3), async {
            while provider.lock().unwrap().cancellations == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| {
            eyre!(
                "paused provider remained open after subtree interruption; evidence {}",
                artifact.display()
            )
        })?;
    }
    let provider = provider.lock().unwrap();
    if kind == Journey::Smoke {
        assert_eq!(provider.log.len(), 1);
        assert_eq!(provider.connections.len(), 1);
        assert_eq!(provider.log[0]["family"], "claude");
        assert_eq!(provider.log[0]["request"]["stream"], true);
    } else if kind == Journey::SiblingMessages {
        let mut exchange = Vec::new();
        for (label, family, marker) in [
            ("sibling-codex-message", "codex", "CLAUDE_TO_CODEX_"),
            ("sibling-claude-reply", "claude", "CODEX_TO_CLAUDE_"),
        ] {
            let delivery = provider
                .log
                .iter()
                .find(|entry| entry["label"] == label && entry["stage"] == 0)
                .expect("missing sibling delivery");
            assert_eq!(delivery["family"], family);
            let delivered = user_text(&delivery["request"]);
            assert!(delivered.contains(marker));
            let metadata = |field| -> u64 {
                delivered
                    .rsplit_once(field)
                    .unwrap()
                    .1
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap()
            };
            exchange.push(json!({"recipient_harness":family,"payload":delivered.rsplit_once("\nMessage body:\n").unwrap().1.lines().next().unwrap(),"message_id":metadata("Message ID: "),"thread_id":metadata("Thread ID: ")}));
        }
        assert_eq!(exchange[0]["thread_id"], exchange[1]["thread_id"]);
        assert_ne!(exchange[0]["message_id"], exchange[1]["message_id"]);
        std::fs::write(
            artifact.join("sibling-exchange.json"),
            serde_json::to_vec_pretty(
                &json!({"exchange":exchange,"transport":"real CLI; Codex Responses WebSocket and Claude Messages SSE; only inference synthetic","provider_trace":"provider.json"}),
            )?,
        )?;
        let completed = provider
            .log
            .iter()
            .find(|entry| entry["label"] == "root" && entry["stage"] == 2)
            .unwrap();
        assert!(
            completed["tool_result"]
                .to_string()
                .contains("SIBLING_EXCHANGE_OK")
        );
    } else if kind == Journey::StaleDirect {
        assert!(
            !artifact.join("workspace/stale-direct.txt").exists(),
            "direct hidden tool executed"
        );
        assert_eq!(
            std::fs::read_to_string(artifact.join("workspace/recovered.txt"))?,
            "recovered"
        );
        assert_eq!(provider.log.len(), 3);
        assert!(
            provider.log[1]["tool_result"]
                .to_string()
                .contains("Code Mode"),
            "missing direct-call rejection receipt"
        );
    } else if kind == Journey::MissingChildAuth {
        assert_eq!(
            provider.log.len(),
            2,
            "unauthorized child contacted a model provider"
        );
        assert!(
            provider
                .connections
                .iter()
                .all(|connection| connection["family"] == family),
            "unauthorized child opened a provider connection"
        );
        assert!(
            provider.log[1]["tool_result"]
                .to_string()
                .contains("auth-denied-ok")
        );
    } else if kind == Journey::WaitLoop {
        let report = provider
            .log
            .iter()
            .find(|entry| entry["label"] == "root" && entry["stage"] == 1)
            .map(|entry| entry["tool_result"].to_string())
            .unwrap_or_default();
        assert!(
            report.contains("wait-loop-ok"),
            "wait loop cell failed: {report}; evidence {}",
            artifact.display()
        );
        assert!(
            provider
                .log
                .iter()
                .any(|entry| entry["label"] == "wait-slow"),
            "running child never dispatched; evidence {}",
            artifact.display()
        );
    } else {
        for (label, expected_family) in [
            ("root", family),
            ("child", if family == "codex" { "claude" } else { "codex" }),
            ("grandchild", family),
            ("inherited", family),
            (
                "followup",
                if family == "codex" { "claude" } else { "codex" },
            ),
        ] {
            let calls: Vec<_> = provider
                .log
                .iter()
                .filter(|call| call["label"] == label)
                .collect();
            assert!(
                !calls.is_empty(),
                "{label} never dispatched; evidence {}",
                artifact.display()
            );
            for call in calls {
                assert_eq!(call["family"], expected_family);
                assert_eq!(call["model"], model(expected_family));
            }
        }
        for (label, stage, expected) in [
            ("root", 1, "nested-and-inherited-ok"),
            ("root", 2, "reuse-ok"),
            ("root", 3, "closed-subtree-ok"),
            ("root", 4, "paused-subtree-released-ok"),
            ("child", 1, "grandchild-ok"),
            ("child", 2, "contract-recovered-ok"),
        ] {
            let call = provider
                .log
                .iter()
                .find(|call| call["label"] == label && call["stage"] == stage)
                .ok_or_else(|| eyre!("missing {label} stage {stage}"))?;
            assert!(
                call["tool_result"].to_string().contains(expected),
                "{label}/{stage} failed: {}; evidence {}",
                call["tool_result"],
                artifact.display()
            );
        }
        assert_eq!(
            std::fs::read_to_string(artifact.join("workspace/grandchild.txt"))?,
            "grandchild-effect"
        );
        let inherited = provider
            .log
            .iter()
            .find(|call| call["label"] == "inherited")
            .unwrap();
        let request = &inherited["request"];
        let effort = if family == "claude" {
            &request["output_config"]["effort"]
        } else if let Some(reasoning) = request.get("reasoning") {
            &reasoning["effort"]
        } else {
            &request["input"]
                .as_array()
                .unwrap()
                .iter()
                .rev()
                .find(|item| item["type"] == "configuration_update")
                .expect("missing inherited reasoning configuration")["reasoning"]["effort"]
        };
        assert_eq!(
            effort, "medium",
            "same-family null thinking did not inherit parent effort"
        );
        assert_eq!(
            provider.pauses, 1,
            "active grandchild never reached the paused provider"
        );
        assert_eq!(
            provider.cancellations, 1,
            "interruption retained the paused provider transport"
        );
    }
    Ok(())
}

#[tokio::test]
async fn claude_root_needs_only_anthropic_credentials() -> Result<()> {
    journey("claude", Journey::Smoke).await
}
#[tokio::test]
async fn claude_and_codex_siblings_exchange_a_threaded_message_and_reply() -> Result<()> {
    journey("codex", Journey::SiblingMessages).await
}
#[tokio::test]
async fn codex_rejects_stale_direct_tools_and_recovers_through_code_mode() -> Result<()> {
    journey("codex", Journey::StaleDirect).await
}
#[tokio::test]
async fn codex_claude_codex_nested_lifecycle_and_inheritance() -> Result<()> {
    journey("codex", Journey::Mixed).await
}
#[tokio::test]
async fn claude_wait_loop_blocks_on_running_children_and_rejects_stuck_rereads() -> Result<()> {
    journey("claude", Journey::WaitLoop).await
}
#[tokio::test]
async fn claude_codex_claude_nested_lifecycle_and_inheritance() -> Result<()> {
    journey("claude", Journey::Mixed).await
}
#[tokio::test]
async fn cross_family_missing_credentials_has_no_child_dispatch_or_registry_effect() -> Result<()> {
    journey("codex", Journey::MissingChildAuth).await?;
    journey("claude", Journey::MissingChildAuth).await
}

#[tokio::test]
async fn root_selection_and_missing_auth_fail_before_provider_dispatch() -> Result<()> {
    for (name, family, auth, flags) in [
        ("invalid-family", "codex", true, vec!["--harness", "bogus"]),
        (
            "codex-wrong-model",
            "codex",
            true,
            vec!["--harness", "codex", "--model", CLAUDE_MODEL],
        ),
        (
            "claude-wrong-model",
            "claude",
            true,
            vec!["--model", CODEX_MODEL],
        ),
        (
            "conflicting-family",
            "claude",
            true,
            vec!["--harness", "codex"],
        ),
        ("codex-no-auth", "codex", false, vec![]),
        ("claude-no-auth", "claude", false, vec![]),
    ] {
        let artifact = artifact(name)?;
        let provider = Arc::new(Mutex::new(Provider {
            root: family,
            journey: Journey::Smoke,
            counts: HashMap::new(),
            log: vec![],
            artifact: artifact.clone(),
            pauses: 0,
            cancellations: 0,
            connections: vec![],
        }));
        let servers = servers(Arc::clone(&provider)).await?;
        let mut command = command(
            &artifact.join("workspace"),
            &servers,
            family,
            auth,
            auth,
            false,
        );
        command.args(flags).arg("REJECT_ROOT_BEFORE_DISPATCH");
        let output = run(
            command,
            &artifact,
            "nonzero exit, actionable error, zero provider requests",
        )
        .await?;
        assert!(
            !output.status.success(),
            "invalid root succeeded; evidence {}",
            artifact.display()
        );
        assert!(
            provider.lock().unwrap().connections.is_empty(),
            "invalid root opened a provider connection"
        );
        assert!(!output.stderr.is_empty(), "invalid root omitted its error");
        if name == "claude-no-auth" {
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("nanocodex --claude auth login"),
                "missing actionable subscription login hint"
            );
        }
        assert!(
            provider.lock().unwrap().log.is_empty(),
            "invalid root dispatched a provider request; evidence {}",
            artifact.display()
        );
        assert!(!artifact.join("workspace/grandchild.txt").exists());
    }
    Ok(())
}

fn subscription_auth_command(workspace: &Path, config: &Path, action: &str) -> Command {
    let mut command = Command::new(local_cli());
    command
        .current_dir(workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CODEX_HOME", workspace.join("codex-home"))
        .args(["--claude", "--claude-oauth-config"])
        .arg(config)
        .args(["auth", action])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

async fn subscription_login(
    workspace: &Path,
    config: &Path,
    artifact: &Path,
    fixture: &Arc<Mutex<SubscriptionFixture>>,
) -> Result<()> {
    let mut command = subscription_auth_command(workspace, config, "login");
    command.arg("--no-open").stdin(Stdio::piped());
    let evidence = artifact.join("login");
    std::fs::create_dir_all(&evidence)?;
    std::fs::write(
        evidence.join("scenario.json"),
        serde_json::to_vec_pretty(
            &json!({"command":format!("{command:?}"),"stdin":"<redacted-code>#<redacted-state>\\n","expected":"PKCE authorization URL, code exchange, profile validation, encrypted durable authentication","reproduce":"cargo test --locked -p nanocodex-bin --test harness_routing claude_subscription_login_refresh_restart_and_logout -- --nocapture"}),
        )?,
    )?;
    let mut child = command.spawn()?;
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut authorization_url = None;
    let mut process_status = None;
    let result = timeout(LIMIT, async {
        let mut out_open = true;
        let mut err_open = true;
        let login_url = loop {
            let mut out_line = String::new();
            let mut err_line = String::new();
            let (count, is_stdout, line) = tokio::select! {
                result = stdout.read_line(&mut out_line), if out_open => (result?, true, out_line),
                result = stderr.read_line(&mut err_line), if err_open => (result?, false, err_line),
                else => return Err(eyre!("login exited without an authorization URL")),
            };
            if count == 0 {
                if is_stdout {
                    out_open = false;
                } else {
                    err_open = false;
                }
                continue;
            }
            if is_stdout {
                out.extend_from_slice(line.as_bytes());
            } else {
                err.extend_from_slice(line.as_bytes());
            }
            if let Some(url) = line
                .split_whitespace()
                .filter_map(|word| reqwest::Url::parse(word).ok())
                .find(|url| url.path() == "/oauth/authorize")
            {
                break url;
            }
        };
        let query: HashMap<String, String> = login_url
            .query_pairs()
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        authorization_url = Some(login_url);
        if query.get("client_id").map(String::as_str) != Some("synthetic-cli-client")
            || query.get("state").is_none_or(|state| state.len() != 43)
        {
            return Err(eyre!(
                "login authorization URL has invalid client ID or state"
            ));
        }
        fixture.lock().unwrap().login_query = query.clone();
        let mut input = child.stdin.take().unwrap();
        input
            .write_all(format!("synthetic-login-code#{}\n", query["state"]).as_bytes())
            .await?;
        input.shutdown().await?;
        drop(input);
        let (out_result, err_result) =
            tokio::join!(stdout.read_to_end(&mut out), stderr.read_to_end(&mut err));
        out_result?;
        err_result?;
        process_status = Some(child.wait().await?);
        if !process_status.as_ref().unwrap().success() {
            return Err(eyre!("login process failed"));
        }
        Ok::<_, eyre::Report>(())
    })
    .await
    .map_err(|_| eyre!("subscription login timed out"))
    .and_then(|result| result);
    // Capture failures before URL parsing or code exchange as well as success.
    // Reap or terminate the child so even an early error records its outcome.
    let mut cleanup_error = None;
    if process_status.is_none() {
        process_status = child.try_wait().ok().flatten();
        if process_status.is_none() {
            if let Err(error) = child.kill().await {
                cleanup_error = Some(error.to_string());
            }
            process_status = child.try_wait().ok().flatten();
        }
    }
    let safe = |bytes: &[u8]| {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        if let Some(url) = &authorization_url {
            text = text.replace(url.as_str(), "<redacted-authorization-url>");
            for (name, value) in url.query_pairs() {
                if name == "state" {
                    text = text.replace(value.as_ref(), "<redacted-state>");
                }
            }
        }
        // A malformed printed URL may not have reached the parser above.
        for word in text
            .split_whitespace()
            .filter(|word| word.contains("/oauth/authorize"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
        {
            text = text.replace(&word, "<redacted-authorization-url>");
        }
        for secret in [
            "synthetic-login-code",
            "synthetic-access-1",
            "synthetic-access-2",
            "synthetic-refresh-1",
            "synthetic-refresh-2",
        ] {
            text = text.replace(secret, "<redacted>");
        }
        text
    };
    std::fs::write(evidence.join("stdout.log"), safe(&out))?;
    std::fs::write(evidence.join("stderr.log"), safe(&err))?;
    std::fs::write(
        evidence.join("outcome.json"),
        serde_json::to_vec_pretty(&json!({
            "success":result.is_ok(),"exit_code":process_status.as_ref().and_then(|status| status.code()),
            "error":result.as_ref().err().map(ToString::to_string),"cleanup_error":cleanup_error,
            "authorization_url_observed":authorization_url.is_some(),
            "pkce":result.is_ok().then_some("S256 challenge checked against token exchange verifier"),
            "stdout":"stdout.log","stderr":"stderr.log","oauth_trace":"../oauth-http.json"
        }))?,
    )?;
    eprintln!("journey evidence: {}", evidence.display());
    result.map_err(|error| eyre!("{error}; evidence {}", evidence.display()))
}

async fn subscription_step(
    command: Command,
    artifact: &Path,
    name: &str,
    expected: &str,
    fixture: &Arc<Mutex<SubscriptionFixture>>,
    provider: &Arc<Mutex<Provider>>,
) -> Result<Output> {
    let evidence = artifact.join(name);
    std::fs::create_dir_all(&evidence)?;
    let output = run(command, &evidence, expected).await?;
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            !text.contains("synthetic-access-") && !text.contains("synthetic-refresh-"),
            "CLI leaked subscription credentials; evidence {}",
            evidence.display()
        );
    }
    std::fs::write(
        evidence.join("provider.json"),
        serde_json::to_vec_pretty(&provider.lock().unwrap().log)?,
    )?;
    let state = fixture.lock().unwrap();
    std::fs::write(
        evidence.join("oauth-http.json"),
        serde_json::to_vec_pretty(&state.oauth)?,
    )?;
    std::fs::write(
        evidence.join("messages-http.json"),
        serde_json::to_vec_pretty(&state.messages)?,
    )?;
    Ok(output)
}

fn encrypted_subscription_store(path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)?;
    for secret in [
        "synthetic-access-",
        "synthetic-refresh-",
        "synthetic-login-code",
    ] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "credential store contains plaintext secrets"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(path)?.permissions().mode() & 0o777,
            0o600,
            "credential store must be owner-only"
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())?
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "credential directory must be owner-only"
        );
    }
    Ok(())
}

#[tokio::test]
async fn claude_subscription_login_recovers_the_same_codex_process() -> Result<()> {
    let artifact = artifact("claude-subscription-recovery")?;
    let workspace = artifact.join("workspace");
    let config = workspace.join("claude-oauth.json");
    let provider = Arc::new(Mutex::new(Provider {
        root: "codex",
        journey: Journey::SubscriptionRecovery,
        counts: HashMap::new(),
        log: vec![],
        artifact: artifact.clone(),
        pauses: 0,
        cancellations: 0,
        connections: vec![],
    }));
    let fixture = Arc::new(Mutex::new(SubscriptionFixture {
        artifact: artifact.clone(),
        oauth: vec![],
        messages: vec![],
        bodies: vec![],
        login_query: HashMap::new(),
        tokens: 0,
        reject_next: false,
    }));
    let gate = Arc::new(SubscriptionRecoveryGate::default());
    let servers = subscription_servers_with_recovery(
        Arc::clone(&provider),
        Some(Arc::clone(&fixture)),
        Some(Arc::clone(&gate)),
    )
    .await?;
    let base = servers.claude.strip_suffix("/v1/messages").unwrap();
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "authorize_url":format!("{base}/oauth/authorize"),"token_url":format!("{base}/oauth/token"),"profile_url":format!("{base}/oauth/profile"),"manual_redirect_uri":format!("{base}/oauth/callback"),"client_id":"synthetic-cli-client","scopes":["user:profile","user:inference","user:sessions:claude_code"],"allow_loopback_http":true
        }))?,
    )?;
    let root_evidence = artifact.join("live-codex-root");
    std::fs::create_dir_all(&root_evidence)?;
    let mut invocation = command(&workspace, &servers, "codex", true, false, false);
    invocation.arg("--claude-oauth-config").arg(&config).args([
        "--model",
        CODEX_MODEL,
        "SUBSCRIPTION_RECOVERY_ROOT",
    ]);
    let mut root = Box::pin(subscription_step(
        invocation,
        &root_evidence,
        "same-process",
        "first Claude spawn rejected without effects; second-process OAuth login; same live Codex process retries successfully with one Claude child and native Write effect",
        &fixture,
        &provider,
    ));
    tokio::select! {
        output = &mut root => {
            let output = output?;
            return Err(eyre!("Codex exited before the signed-out boundary ({:?}); evidence {}", output.status, artifact.display()));
        }
        denied = timeout(LIMIT, gate.denied.notified()) => {
            denied.map_err(|_| eyre!("signed-out child boundary timed out; evidence {}", artifact.display()))?;
        }
    }
    let boundary: Value =
        serde_json::from_slice(&std::fs::read(artifact.join("signed-out-boundary.json"))?)?;
    let denied_result = boundary["observed_tool_result"].to_string();
    assert!(
        denied_result.contains("subscription-login-required-ok")
            && denied_result.contains("nanocodex --claude auth login")
            && !denied_result.contains("Script failed"),
        "first spawn must fail with a login hint and no registry child: {denied_result}; evidence {}",
        artifact.display()
    );
    assert!(fixture.lock().unwrap().oauth.is_empty());
    assert!(fixture.lock().unwrap().messages.is_empty());
    assert!(
        provider
            .lock()
            .unwrap()
            .connections
            .iter()
            .all(|connection| connection["family"] != "claude"),
        "signed-out child opened a Claude connection; evidence {}",
        artifact.display()
    );
    subscription_login(&workspace, &config, &artifact, &fixture).await?;
    encrypted_subscription_store(&workspace.join("codex-home/claude/private/auth"))?;
    gate.logged_in.notify_one();
    let output = root.await?;
    success(
        &output,
        &root_evidence.join("same-process"),
        "subscription-recovery-answer",
    )?;
    let retry = provider
        .lock()
        .unwrap()
        .log
        .iter()
        .find(|call| call["family"] == "codex" && call["label"] == "root" && call["stage"] == 2)
        .map(|call| call["tool_result"].clone())
        .ok_or_else(|| {
            eyre!(
                "missing same-process retry result; evidence {}",
                artifact.display()
            )
        })?;
    let effect = std::fs::read_to_string(workspace.join("subscription-child.txt")).ok();
    let retry_text = retry.to_string();
    let recovered = retry_text.contains("subscription-same-process-recovered-ok")
        && !retry_text.contains("Script failed");
    let (tokens, messages) = {
        let state = fixture.lock().unwrap();
        (state.tokens, state.messages.len())
    };
    std::fs::write(
        artifact.join("recovery-contract.json"),
        serde_json::to_vec_pretty(&json!({
            "reproduce":"cargo test --locked -p nanocodex-bin --test harness_routing claude_subscription_login_recovers_the_same_codex_process -- --nocapture",
            "expected":{"signed_out":"login hint, no retained child, no OAuth or Claude connection","login":"real second-process CLI PKCE exchange and profile validation to same encrypted store","retry":"same running Codex process admits exactly one Claude child, native Write effect and structured result"},
            "observed":{"same_process_recovered":recovered,"retry_tool_result":retry,"native_effect":effect,"token_exchanges":tokens,"messages_requests":messages},
            "signed_out_boundary":"signed-out-boundary.json","login":"login/outcome.json","root":"live-codex-root/same-process/outcome.json","provider_trace":"provider.json","oauth_trace":"oauth-http.json","messages_trace":"messages-http.json"
        }))?,
    )?;
    assert!(
        recovered,
        "same live Codex process could not spawn Claude after successful login: {retry}; evidence {}",
        artifact.display()
    );
    assert_eq!(effect.as_deref(), Some("subscription-child-effect"));
    assert_eq!(tokens, 1);
    assert!(messages > 0);
    Ok(())
}

#[tokio::test]
async fn claude_subscription_login_refresh_restart_and_logout() -> Result<()> {
    let artifact = artifact("claude-subscription")?;
    let workspace = artifact.join("workspace");
    let config = workspace.join("claude-oauth.json");
    let journal = workspace.join("codex-home/claude");
    std::fs::create_dir_all(&journal)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o755))?;
    }
    let provider = Arc::new(Mutex::new(Provider {
        root: "claude",
        journey: Journey::Subscription,
        counts: HashMap::new(),
        log: vec![],
        artifact: artifact.clone(),
        pauses: 0,
        cancellations: 0,
        connections: vec![],
    }));
    let fixture = Arc::new(Mutex::new(SubscriptionFixture {
        artifact: artifact.clone(),
        oauth: vec![],
        messages: vec![],
        bodies: vec![],
        login_query: HashMap::new(),
        tokens: 0,
        reject_next: true,
    }));
    let servers = subscription_servers(Arc::clone(&provider), Some(Arc::clone(&fixture))).await?;
    let base = servers.claude.strip_suffix("/v1/messages").unwrap();
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "authorize_url":format!("{base}/oauth/authorize"),"token_url":format!("{base}/oauth/token"),"profile_url":format!("{base}/oauth/profile"),"manual_redirect_uri":format!("{base}/oauth/callback"),"client_id":"synthetic-cli-client","scopes":["user:profile","user:inference","user:sessions:claude_code"],"refresh_margin_millis":300000,"login_ttl_millis":600000,"allow_loopback_http":true
        }))?,
    )?;
    let mut invalid = Command::new(local_cli());
    invalid
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CODEX_HOME", workspace.join("codex-home"))
        .args([
            "--prompt",
            "must-not-start-agent",
            "--claude",
            "auth",
            "status",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let invalid = subscription_step(
        invalid,
        &artifact,
        "interactive-flag-before-auth",
        "exit 2 naming --prompt; no OAuth or model provider request/connection",
        &fixture,
        &provider,
    )
    .await?;
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("--prompt"));
    assert!(fixture.lock().unwrap().oauth.is_empty());
    assert!(fixture.lock().unwrap().messages.is_empty());
    assert!(provider.lock().unwrap().connections.is_empty());
    subscription_login(&workspace, &config, &artifact, &fixture).await?;
    let store = journal.join("private/auth");
    encrypted_subscription_store(&store)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&journal)?.permissions().mode() & 0o777,
            0o755,
            "subscription login changed existing journal directory permissions"
        );
    }
    let mut status = subscription_auth_command(&workspace, &config, "status");
    status.env("NANOCODEX_CLAUDE_AUTH_FILE", &store);
    let output = subscription_step(
        status,
        &artifact,
        "authenticated-status",
        "authenticated safe JSON status",
        &fixture,
        &provider,
    )
    .await?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["state"],
        "authenticated"
    );

    let invocation = |family: &str| {
        let mut command = command(
            &workspace,
            &servers,
            family,
            family == "codex",
            false,
            false,
        );
        command.arg("--claude-oauth-config").arg(&config).args([
            "--model",
            model(family),
            "SUBSCRIPTION_ROOT",
        ]);
        command
    };
    let native = subscription_step(
        invocation("claude"),
        &artifact,
        "native-root",
        "native Write effect, one 401 refresh and identical-byte retry",
        &fixture,
        &provider,
    )
    .await?;
    success(
        &native,
        &artifact.join("native-root"),
        "subscription-native-answer",
    )?;
    assert_eq!(
        std::fs::read_to_string(workspace.join("grandchild.txt"))?,
        "grandchild-effect"
    );
    {
        let state = fixture.lock().unwrap();
        assert_eq!(state.tokens, 2);
        assert_eq!(state.messages[0]["status"], 401);
        assert_eq!(state.messages[1]["status"], 200);
        assert_eq!(
            state.bodies[0], state.bodies[1],
            "refresh changed frozen Messages request bytes"
        );
        assert_eq!(state.messages[0]["token_generation"], 1);
        assert_eq!(state.messages[1]["token_generation"], 2);
    }
    encrypted_subscription_store(&store)?;
    {
        let mut state = provider.lock().unwrap();
        state.root = "codex";
        state.counts.clear();
    }
    let mixed = subscription_step(
        invocation("codex"),
        &artifact,
        "mixed-restart",
        "new Codex process uses persisted subscription for Claude child and custom Code Mode tool",
        &fixture,
        &provider,
    )
    .await?;
    success(
        &mixed,
        &artifact.join("mixed-restart"),
        "subscription-mixed-answer",
    )?;
    {
        let state = provider.lock().unwrap();
        let parent_result = state
            .log
            .iter()
            .find(|call| call["family"] == "codex" && call["label"] == "root" && call["stage"] == 1)
            .ok_or_else(|| {
                eyre!(
                    "missing mixed parent completion tool result; evidence {}",
                    artifact.display()
                )
            })?["tool_result"]
            .to_string();
        assert!(
            parent_result.contains("subscription-child-ok"),
            "parent did not validate child completion: {parent_result}; evidence {}",
            artifact.display()
        );
        assert!(
            !parent_result.contains("Script failed"),
            "parent Code Mode failed: {parent_result}; evidence {}",
            artifact.display()
        );
    }
    assert_eq!(
        std::fs::read_to_string(workspace.join("subscription-child.txt"))?,
        "subscription-child-effect"
    );
    {
        let state = fixture.lock().unwrap();
        assert_eq!(
            state.tokens, 2,
            "restart unnecessarily exchanged credentials"
        );
        let mut first_device: Option<String> = None;
        let mut sessions = HashMap::<String, String>::new();
        for call in &state.messages {
            let request = &call["request"];
            let identity: Value =
                serde_json::from_str(request["metadata"]["user_id"].as_str().unwrap())?;
            assert_eq!(identity["account_uuid"], "synthetic-account");
            let device = identity["device_id"].as_str().unwrap();
            assert_eq!(device.len(), 64);
            if let Some(previous) = &first_device {
                assert_eq!(
                    device,
                    previous.as_str(),
                    "installation identity changed across root/child/restart"
                );
            } else {
                first_device = Some(device.to_owned());
            }
            let session = identity["session_id"].as_str().unwrap();
            assert!(!session.is_empty());
            let label = label(request);
            if let Some(previous) = sessions.insert(label, session.to_owned()) {
                assert_eq!(
                    session, previous,
                    "agent session identity changed across turns/retry"
                );
            }
            let tools = request["tools"].as_array().unwrap();
            assert!(
                tools.iter().any(|tool| tool["name"] == "_exec"),
                "Code Mode tool lacks subscription wire prefix"
            );
        }
        assert_ne!(
            sessions["root"], sessions["child"],
            "root and child shared session affinity"
        );
    }
    let mut logout_command = subscription_auth_command(&workspace, &config, "logout");
    logout_command.arg("--claude-auth-file").arg(&store);
    let logout = subscription_step(
        logout_command,
        &artifact,
        "logout",
        "durable signed-out state and refresh token revocation",
        &fixture,
        &provider,
    )
    .await?;
    assert!(logout.status.success());
    encrypted_subscription_store(&store)?;
    let status = subscription_auth_command(&workspace, &config, "status");
    let output = subscription_step(
        status,
        &artifact,
        "signed-out-status",
        "safe signed_out JSON after process restart",
        &fixture,
        &provider,
    )
    .await?;
    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout)?["state"],
        "signed_out"
    );
    let before = provider.lock().unwrap().connections.len();
    let denied = subscription_step(
        invocation("claude"),
        &artifact,
        "logged-out-root",
        "nonzero exit with nanocodex --claude auth login, zero provider connections",
        &fixture,
        &provider,
    )
    .await?;
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("nanocodex --claude auth login"));
    assert_eq!(provider.lock().unwrap().connections.len(), before);
    let claude_before = fixture.lock().unwrap().messages.len();
    let claude_connections_before = provider
        .lock()
        .unwrap()
        .connections
        .iter()
        .filter(|connection| connection["family"] == "claude")
        .count();
    {
        let mut state = provider.lock().unwrap();
        state.journey = Journey::MissingChildAuth;
        state.counts.clear();
    }
    let denied = subscription_step(
        invocation("codex"),
        &artifact,
        "logged-out-child",
        "Claude child refused with login hint, no registry effect or Claude dispatch",
        &fixture,
        &provider,
    )
    .await?;
    success(
        &denied,
        &artifact.join("logged-out-child"),
        "auth-denied-answer",
    )?;
    assert_eq!(fixture.lock().unwrap().messages.len(), claude_before);
    assert_eq!(
        provider
            .lock()
            .unwrap()
            .connections
            .iter()
            .filter(|connection| connection["family"] == "claude")
            .count(),
        claude_connections_before,
        "logged-out child connected to Claude before authorization"
    );
    assert!(
        provider.lock().unwrap().log.last().unwrap()["tool_result"]
            .to_string()
            .contains("auth-denied-ok")
    );
    assert_eq!(
        fixture
            .lock()
            .unwrap()
            .oauth
            .iter()
            .map(|call| call["endpoint"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["token", "profile", "token", "profile", "revoke"]
    );
    std::fs::write(
        artifact.join("subscription-contract.json"),
        serde_json::to_vec_pretty(&json!({
            "observed":{"pkce":"S256 matches exchange verifier","messages_retry":"401 then 200, exact request bytes equal","token_exchanges":2,"native_effect":"grandchild.txt = grandchild-effect","child_effect":"subscription-child.txt = subscription-child-effect","restart":"authenticated JSON status and inference with no additional token exchange","identity":"stable device/account across processes, stable session within agent, distinct root/child sessions","store":"encrypted credentials contain no synthetic plaintext tokens; auth permissions 0600 and private directory 0700; existing journal directory retains 0755","logout":"signed_out JSON status, root login hint before dispatch, child login hint without registry or transport effects"},
            "provider_trace":"provider.json","oauth_trace":"oauth-http.json","messages_trace":"messages-http.json"
        }))?,
    )?;
    Ok(())
}

#[tokio::test]
async fn claude_durable_terminal_replays_in_a_second_process_without_a_provider_request()
-> Result<()> {
    let artifact = artifact("claude-durable-replay")?;
    let workspace = artifact.join("workspace");
    let database = workspace.join("claude.sqlite3");
    let provider = Arc::new(Mutex::new(Provider {
        root: "claude",
        journey: Journey::Smoke,
        counts: HashMap::new(),
        log: vec![],
        artifact: artifact.clone(),
        pauses: 0,
        cancellations: 0,
        connections: vec![],
    }));
    let servers = servers(Arc::clone(&provider)).await?;
    let invocation = || {
        let mut command = command(&workspace, &servers, "claude", false, true, false);
        command
            .args(["--model", CLAUDE_MODEL, "--local-durability"])
            .arg(&database)
            .args([
                "--local-durability-state-id",
                "synthetic-claude-root",
                "--request-id",
                "synthetic-claude-turn",
                "DURABLE_CLAUDE_ROOT",
            ]);
        command
    };
    let first = artifact.join("initial");
    std::fs::create_dir_all(&first)?;
    let initial = run(
        invocation(),
        &first,
        "durably completed Claude Messages answer",
    )
    .await?;
    success(&initial, &first, "claude-only-answer")?;
    assert_eq!(provider.lock().unwrap().log.len(), 1);
    let initial_connections = provider.lock().unwrap().connections.len();
    assert_eq!(initial_connections, 1);
    std::fs::write(
        first.join("provider.json"),
        serde_json::to_vec_pretty(&provider.lock().unwrap().log)?,
    )?;
    assert!(
        database.exists(),
        "CLI did not create its requested durability store"
    );
    let second = artifact.join("replay");
    std::fs::create_dir_all(&second)?;
    let replay = run(
        invocation(),
        &second,
        "single terminal replay event, zero new provider requests",
    )
    .await?;
    success(&replay, &second, "claude-only-answer")?;
    let events: Vec<Value> = String::from_utf8_lossy(&replay.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(events.len(), 1, "durable replay emitted generation events");
    assert_eq!(events[0]["type"], "run.completed");
    assert_eq!(events[0]["payload"]["model_calls"], 0);
    assert_eq!(
        provider.lock().unwrap().log.len(),
        1,
        "second process dispatched a Claude Messages request"
    );
    assert_eq!(
        provider.lock().unwrap().connections.len(),
        initial_connections,
        "second process connected to a provider during terminal replay"
    );
    std::fs::write(second.join("provider.json"), "[]\n")?;
    std::fs::write(second.join("connections.json"), "[]\n")?;
    Ok(())
}

fn wire_instructions(request: &Value) -> String {
    if let Some(system) = request.get("system") {
        return match system {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
    }
    if let Some(text) = request["instructions"].as_str() {
        return text.into();
    }
    // Codex carries its primary instruction in the first developer message.
    // Later developer messages retain independent host permissions/project data.
    if let Some(message) = request["input"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| item["role"] == "developer" && item["type"] == "message")
    }) {
        return message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
    }
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["role"] == "system")
        .map(|item| item["content"].as_str().unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

fn context_files(workspace: &Path) -> Result<()> {
    std::fs::create_dir_all(workspace.join(".claude/skills/claude-fixture"))?;
    std::fs::create_dir_all(workspace.join(".agents/skills/fixture"))?;
    std::fs::write(workspace.join("AGENTS.md"), "fixture-agents-context\n")?;
    std::fs::write(workspace.join("CLAUDE.md"), "fixture-claude-context\n")?;
    std::fs::write(
        workspace.join(".claude/CLAUDE.md"),
        "fixture-dotclaude-context\n",
    )?;
    std::fs::write(
        workspace.join(".agents/skills/fixture/SKILL.md"),
        "---\nname: fixture\ndescription: Lazy skill fixture\n---\nfixture-lazy-skill-body\n",
    )?;
    std::fs::write(
        workspace.join(".claude/skills/claude-fixture/SKILL.md"),
        "---\nname: claude-fixture\ndescription: Claude skill fixture\n---\nfixture-claude-skill-body\n",
    )?;
    // These files must never enter native automatic workspace context.
    std::fs::write(
        workspace.parent().unwrap().join("AGENTS.md"),
        "fixture-ancestor-denied",
    )?;
    std::fs::write(workspace.join("home/CLAUDE.md"), "fixture-home-denied")?;
    std::fs::create_dir_all(workspace.join("nested"))?;
    std::fs::write(workspace.join("nested/CLAUDE.md"), "fixture-nested-denied")?;
    Ok(())
}

fn context_command(workspace: &Path, servers: &Servers, family: &str) -> Command {
    command(workspace, servers, family, true, true, false)
}

#[tokio::test]
async fn native_cli_project_context_is_bounded_lazy_and_explicitly_replaceable() -> Result<()> {
    {
        let family = "claude";
        let artifact = artifact(&format!("{family}-project-context"))?;
        let workspace = artifact.join("workspace");
        context_files(&workspace)?;
        let provider = Arc::new(Mutex::new(Provider {
            root: family,
            journey: Journey::ProjectContext,
            counts: HashMap::new(),
            log: vec![],
            artifact: artifact.clone(),
            pauses: 0,
            cancellations: 0,
            connections: vec![],
        }));
        let servers = servers(Arc::clone(&provider)).await?;
        let mut invocation = context_command(&workspace, &servers, family);
        invocation.arg("PROJECT_CONTEXT_ROOT");
        let output = run(invocation, &artifact, "workspace context transmitted; skill body loaded only by native Read/read_file; native Write/write creates context-effect.txt").await?;
        success(&output, &artifact, "project-context-answer")?;
        assert_eq!(
            std::fs::read_to_string(workspace.join("context-effect.txt"))?,
            "context-effect"
        );
        {
            let provider = provider.lock().unwrap();
            let initial = &provider.log[0]["request"];
            let system = wire_instructions(initial);
            assert!(system.contains("fixture-agents-context"));
            assert_eq!(
                system.contains("fixture-claude-context"),
                family == "claude"
            );
            assert_eq!(
                system.contains("fixture-dotclaude-context"),
                family == "claude"
            );
            assert!(system.contains(".agents/skills/fixture/SKILL.md"));
            assert_eq!(
                system.contains(".claude/skills/claude-fixture/SKILL.md"),
                family == "claude"
            );
            for excluded in [
                "fixture-lazy-skill-body",
                "fixture-claude-skill-body",
                "fixture-ancestor-denied",
                "fixture-home-denied",
                "fixture-nested-denied",
            ] {
                assert!(
                    !system.contains(excluded),
                    "automatic context leaked {excluded}"
                );
            }
            assert!(
                provider.log[1]["tool_result"]
                    .to_string()
                    .contains("fixture-lazy-skill-body")
            );
            let names = initial["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                names.into_iter().collect::<std::collections::BTreeSet<_>>(),
                ["exec", "wait"].into_iter().collect()
            );
            assert!(!initial["tools"].as_array().unwrap().iter().any(|tool| {
                tool["type"]
                    .as_str()
                    .unwrap_or_default()
                    .starts_with("web_search")
            }));
        }
        // New process with an exact caller replacement, including enabled subagents.
        let override_path = artifact.join("override");
        std::fs::create_dir_all(&override_path)?;
        let mut invocation = context_command(&workspace, &servers, family);
        invocation.args([
            "--instructions",
            "fixture-explicit-instructions",
            "OVERRIDE_ROOT",
        ]);
        let output = run(invocation, &override_path, "explicit instructions replace all default prompt modules and automatic context; tool catalog remains usable").await?;
        success(&output, &override_path, "project-context-answer")?;
        assert_eq!(
            wire_instructions(&provider.lock().unwrap().log.last().unwrap()["request"]),
            "fixture-explicit-instructions"
        );

        // Truncation is visible on the wire, and large skill bodies stay lazy.
        let bounded_path = artifact.join("bounded");
        std::fs::create_dir_all(&bounded_path)?;
        std::fs::write(
            workspace.join("AGENTS.md"),
            format!(
                "fixture-bounded-start{}fixture-bounded-end",
                "a".repeat(64_000)
            ),
        )?;
        for n in 0..80 {
            let skill = workspace.join(format!(".agents/skills/bounded-{n:02}"));
            std::fs::create_dir_all(&skill)?;
            std::fs::write(
                skill.join("SKILL.md"),
                format!(
                    "---\nname: bounded-{n:02}\ndescription: Bounded skill fixture\n---\nfixture-unloaded-skill-body"
                ),
            )?;
        }
        let mut invocation = context_command(&workspace, &servers, family);
        invocation.arg("BOUNDED_ROOT");
        let output = run(invocation, &bounded_path, "family-bounded project excerpts and skill catalog, visible truncation, no skill bodies").await?;
        success(&output, &bounded_path, "project-context-answer")?;
        let system = wire_instructions(&provider.lock().unwrap().log.last().unwrap()["request"]);
        assert!(system.contains("fixture-bounded-start"));
        assert!(!system.contains("fixture-bounded-end"));
        assert!(system.contains("\"truncated\":true"));
        assert_eq!(
            system.matches("/SKILL.md").count(),
            if family == "claude" { 64 } else { 32 }
        );
        assert!(!system.contains("fixture-unloaded-skill-body"));
        assert!(system.len() < if family == "claude" { 80_000 } else { 40_000 });
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn native_cli_project_context_rejects_symlinks_and_special_files() -> Result<()> {
    use std::os::unix::fs::symlink;
    let artifact = artifact("native-project-context-symlinks")?;
    let workspace = artifact.join("workspace");
    let outside = artifact.join("outside");
    std::fs::create_dir_all(outside.join("skills/escaped"))?;
    std::fs::write(outside.join("AGENTS.md"), "fixture-symlink-secret")?;
    std::fs::write(outside.join("CLAUDE.md"), "fixture-symlink-secret")?;
    std::fs::write(
        outside.join("skills/escaped/SKILL.md"),
        "fixture-symlink-skill-secret",
    )?;
    symlink(outside.join("AGENTS.md"), workspace.join("AGENTS.md"))?;
    symlink(&outside, workspace.join(".claude"))?;
    std::fs::create_dir_all(workspace.join(".agents/skills"))?;
    symlink(
        outside.join("skills/escaped"),
        workspace.join(".agents/skills/escaped"),
    )?;
    nix::unistd::mkfifo(&workspace.join("CLAUDE.md"), nix::sys::stat::Mode::S_IRUSR)?;
    let provider = Arc::new(Mutex::new(Provider {
        root: "claude",
        journey: Journey::Smoke,
        counts: HashMap::new(),
        log: vec![],
        artifact: artifact.clone(),
        pauses: 0,
        cancellations: 0,
        connections: vec![],
    }));
    let servers = servers(Arc::clone(&provider)).await?;
    {
        let family = "claude";
        let path = artifact.join(family);
        std::fs::create_dir_all(&path)?;
        let mut invocation = context_command(&workspace, &servers, family);
        invocation.arg("SYMLINK_ROOT");
        let output = run(invocation, &path, "symlinked file/directory/skill and FIFO excluded; CLI reaches provider and completes without blocking").await?;
        success(&output, &path, "claude-only-answer")?;
        let system = wire_instructions(&provider.lock().unwrap().log.last().unwrap()["request"]);
        assert!(!system.contains("fixture-symlink"));
        assert!(!system.contains("escaped/SKILL.md"));
    }
    Ok(())
}

#[tokio::test]
async fn native_cli_cross_family_children_resolve_defaults_and_preserve_explicit_override()
-> Result<()> {
    let mut defaults = HashMap::<String, String>::new();
    for family in ["claude", "codex"] {
        for custom in [false, true] {
            let artifact = artifact(&format!("{family}-context-routing-{custom}"))?;
            let workspace = artifact.join("workspace");
            context_files(&workspace)?;
            let provider = Arc::new(Mutex::new(Provider {
                root: family,
                journey: Journey::ContextRouting,
                counts: HashMap::new(),
                log: vec![],
                artifact: artifact.clone(),
                pauses: 0,
                cancellations: 0,
                connections: vec![],
            }));
            let servers = servers(Arc::clone(&provider)).await?;
            let mut invocation = context_command(&workspace, &servers, family);
            if custom {
                invocation.args(["--instructions", "fixture-cross-family-override"]);
            }
            invocation.arg("CONTEXT_ROUTING_ROOT");
            let output = run(invocation, &artifact, "root spawns Claude and Codex children; family defaults independent of parent, explicit replacement inherited without context").await?;
            success(&output, &artifact, "context-routing-answer")?;
            let provider = provider.lock().unwrap();
            assert!(
                provider.log.last().unwrap()["tool_result"]
                    .to_string()
                    .contains("context-routing-ok")
            );
            for target in ["claude", "codex"] {
                let call = provider
                    .log
                    .iter()
                    .find(|call| call["family"] == target && call["label"] == "child")
                    .expect("missing child dispatch");
                let system = wire_instructions(&call["request"]);
                if custom {
                    if family == "codex" && target == "codex" {
                        // Codex retains its host-authored delegation guidance with an
                        // explicit primary instruction; same-family children inherit
                        // that exact effective instruction observed on the wire.
                        let root = provider
                            .log
                            .iter()
                            .find(|call| call["label"] == "root")
                            .expect("missing root dispatch");
                        let inherited = wire_instructions(&root["request"]);
                        assert!(inherited.starts_with("fixture-cross-family-override"));
                        assert!(!inherited.contains("fixture-agents-context"));
                        assert!(!inherited.contains("fixture-claude-context"));
                        assert_eq!(system, inherited);
                    } else if target == "codex" {
                        // Every Codex request appends the current model identity,
                        // including children launched by a Claude root.
                        let (instructions, identity) = system
                            .split_once("\n\n<runtime_model_identity>")
                            .expect("Codex child omitted runtime model identity");
                        assert_eq!(instructions, "fixture-cross-family-override");
                        assert!(
                            identity.contains(&format!(
                                "model_id: {}",
                                call["model"].as_str().unwrap()
                            ))
                        );
                        assert!(!system.contains("fixture-agents-context"));
                        assert!(!system.contains("fixture-claude-context"));
                    } else {
                        assert_eq!(system, "fixture-cross-family-override");
                    }
                } else if target != "codex" {
                    assert!(system.contains("fixture-agents-context"));
                    assert_eq!(
                        system.contains("fixture-claude-context"),
                        target == "claude"
                    );
                    if let Some(previous) = defaults.insert(target.into(), system.clone()) {
                        assert_eq!(
                            system, previous,
                            "child defaults changed with parent family"
                        );
                    }
                    if target == family {
                        assert_eq!(
                            system,
                            wire_instructions(&provider.log[0]["request"]),
                            "same-family child lost root defaults"
                        );
                    }
                } else {
                    assert!(!system.contains("fixture-claude-context"));
                    if family != "codex" {
                        assert_ne!(
                            system,
                            wire_instructions(&provider.log[0]["request"]),
                            "Codex inherited native root defaults"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}
