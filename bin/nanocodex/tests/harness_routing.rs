//! Real CLI journeys; only the external model providers are synthetic.
//! Evidence: output/harness-routing/<journey>-<uuid>/ includes scenario, outcome,
//! provider requests, and CLI output or the raw terminal transcript.
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
    Mixed,
    MissingChildAuth,
    Subscription,
    SubscriptionRecovery,
    XaiMixed,
    ProjectContext,
    ContextRouting,
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
        let reply = if family == "claude" {
            for tool in request["tools"].as_array().unwrap() {
                let name = tool["name"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches('_');
                assert!(
                    !["exec", "wait", "tool_search"].contains(&name),
                    "Claude leaked {name}"
                );
            }
            self.native_script(label, stage, &request)
        } else {
            self.script(label, stage)
        };
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

    // Claude fixtures speak the shipped native catalog. Other providers retain
    // their Code Mode journeys, including the mixed registry lifecycle checks.
    fn native_script(&self, label: &str, stage: usize, request: &Value) -> Reply {
        let call = |name, input| Reply::Native { name, input };
        let submit = |answer| call("SubmitResult", json!({"output":{"answer":answer}}));
        let spawn = |family: Option<&str>, task: &str, background: bool| {
            let mut input = json!({"description":task,"prompt":task,"output_contract":contract(),"run_in_background":background});
            if let Some(family) = family {
                input["harness"] = json!(family);
                input["model"] = json!(model(family));
            }
            call("Agent", input)
        };
        let receipt = native_receipt(request);
        let answer = |expected| {
            assert_eq!(
                receipt["report"]["agents"][0]["status"]["output"]["answer"], expected,
                "native child receipt: {receipt}"
            )
        };
        let error = || {
            assert!(
                native_error(request),
                "expected native tool error: {receipt}"
            )
        };
        if self.journey == Journey::ContextRouting {
            return match (label, stage) {
                ("root", 0..=2) => {
                    if stage > 0 {
                        answer("context-child-answer");
                    }
                    spawn(
                        Some(["claude", "xai", "codex"][stage]),
                        "MIXED_CHILD",
                        false,
                    )
                }
                ("root", _) => {
                    answer("context-child-answer");
                    Reply::Text("context-routing-answer".into())
                }
                ("child", 0) => submit("context-child-answer"),
                _ => Reply::Text("context child finished".into()),
            };
        }
        if self.journey == Journey::XaiMixed {
            return match (label, stage) {
                ("root", 0) => spawn(Some("xai"), "MIXED_CHILD", false),
                ("root", _) => {
                    answer("xai-mixed-child-answer");
                    Reply::Text("xai-mixed-root-answer".into())
                }
                ("grandchild", 0) => submit("xai-mixed-grandchild-answer"),
                _ => Reply::Text("mixed child finished".into()),
            };
        }
        if matches!(
            self.journey,
            Journey::Subscription | Journey::SubscriptionRecovery
        ) {
            return if label == "child" && stage == 1 {
                submit("subscription-child-answer")
            } else {
                self.script(label, stage)
            };
        }
        if self.journey == Journey::MissingChildAuth {
            return match stage {
                0 => spawn(Some("codex"), "AUTH_CHILD", false),
                1 => {
                    error();
                    call("ListAgents", json!({"include_completed":true}))
                }
                _ => {
                    assert_eq!(receipt["agents"].as_array().unwrap().len(), 0);
                    Reply::Text("auth-denied-answer".into())
                }
            };
        }
        if self.journey != Journey::Mixed {
            return self.script(label, stage);
        }
        match (label, stage) {
            ("root", 0) => call(
                "Agent",
                json!({"description":"invalid","prompt":"INVALID_CHILD","harness":"bogus"}),
            ),
            ("root", 1) => {
                error();
                call(
                    "Agent",
                    json!({"description":"invalid","prompt":"INVALID_CHILD","harness":"claude","model":"sol"}),
                )
            }
            ("root", 2) => {
                error();
                call(
                    "Agent",
                    json!({"description":"invalid","prompt":"INVALID_CHILD","harness":"codex","model":"claude-sonnet-4-6"}),
                )
            }
            ("root", 3) => {
                error();
                call("ListAgents", json!({"include_completed":true}))
            }
            ("root", 4) => {
                assert_eq!(receipt["agents"].as_array().unwrap().len(), 0);
                spawn(Some("codex"), "MIXED_CHILD", false)
            }
            ("root", 5) => {
                answer("nested-answer");
                spawn(None, "INHERITED_CHILD", false)
            }
            ("root", 6) => {
                answer("inherited-answer");
                call(
                    "SendMessage",
                    json!({"recipient":"agent-1","content":"FOLLOWUP_CHILD"}),
                )
            }
            ("root", 7) => {
                assert!(!native_error(request));
                call("TaskOutput", json!({"task_id":"agent-1","timeout":20000}))
            }
            ("root", 8) => {
                answer("followup-answer");
                call("CloseAgent", json!({"task_id":"agent-1"}))
            }
            ("root", 9) => {
                let agents = receipt["agents"].as_array().unwrap();
                assert_eq!(agents.len(), 2);
                assert!(agents.iter().all(|a| a["status"]["state"] == "closed"));
                call(
                    "SendMessage",
                    json!({"recipient":"agent-1","content":"cannot revive"}),
                )
            }
            ("root", 10) => {
                error();
                call("ListAgents", json!({"include_completed":true}))
            }
            ("root", 11) => spawn(Some("codex"), "PAUSED_CHILD", true),
            ("root", 12) => {
                assert_eq!(receipt["task_id"], "agent-4");
                call("TaskOutput", json!({"task_id":"agent-4","timeout":1000}))
            }
            ("root", 13) => {
                assert_eq!(receipt["report"]["timed_out"], true);
                assert_eq!(receipt["report"]["agents"][0]["status"]["state"], "running");
                call("TaskStop", json!({"task_id":"agent-4"}))
            }
            ("root", 14) => {
                let agents = receipt["agents"].as_array().unwrap();
                assert_eq!(agents.len(), 2);
                assert!(agents.iter().all(|a| a["status"]["state"] == "interrupted"));
                call("CloseAgent", json!({"task_id":"agent-4"}))
            }
            ("root", _) => {
                let agents = receipt["agents"].as_array().unwrap();
                assert_eq!(agents.len(), 2);
                assert!(agents.iter().all(|a| a["status"]["state"] == "closed"));
                Reply::Text("mixed-routing-answer".into())
            }
            ("child", 0) => spawn(Some("codex"), "MIXED_GRANDCHILD", false),
            ("child", 1) => {
                answer("grandchild-answer");
                call("SubmitResult", json!({"output":{"answer":42}}))
            }
            ("child", 2) => {
                error();
                submit("nested-answer")
            }
            ("child", _) => {
                assert_eq!(receipt["accepted"], true);
                Reply::Text("child finished".into())
            }
            ("followup", 0) => submit("followup-answer"),
            ("followup", _) => Reply::Text("followup finished".into()),
            ("grandchild", 0) => Reply::Write {
                path: "grandchild.txt",
                content: "grandchild-effect",
            },
            ("grandchild", 1) => submit("grandchild-answer"),
            ("grandchild", _) => Reply::Text("grandchild finished".into()),
            ("inherited", 0) => submit("inherited-answer"),
            ("inherited", _) => Reply::Text("inherited finished".into()),
            ("paused-child", 0) => spawn(Some("codex"), "PAUSED_GRANDCHILD", false),
            ("paused-grandchild", _) => Reply::Pause,
            _ => Reply::Text("unexpected native fixture request".into()),
        }
    }

    fn script(&self, label: &str, stage: usize) -> Reply {
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
for (const family of ['claude', 'xai', 'codex']) {{
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
        if self.journey == Journey::XaiMixed {
            return match (label, stage) {
                ("root", 0) | ("child", 0) => {
                    let (family, task, answer) = if label == "root" {
                        ("xai", "MIXED_CHILD", "xai-mixed-child-answer")
                    } else { (self.root, "MIXED_GRANDCHILD", "xai-mixed-grandchild-answer") };
                    Reply::Code(format!(r#"
const c=await tools.spawn_agent({{harness:'{family}',model:'{}',role:'{task}',task:'{task}',thinking:null,output_contract:{}}});
const done=await tools.wait_agent({{agent_ids:[c.agent_id],timeout_ms:20000}});text(done);
if(done.timed_out||done.agents[0].status.output.answer!=='{answer}')throw Error('mixed xAI child failed');text('mixed-xai-ok');
"#, model(family), contract()))
                },
                ("child", 1) => Reply::Code("text(await tools.submit_result({output:{answer:'xai-mixed-child-answer'}}));".into()),
                ("grandchild", 0) => Reply::Code("text(await tools.submit_result({output:{answer:'xai-mixed-grandchild-answer'}}));".into()),
                ("root", _) => Reply::Text("xai-mixed-root-answer".into()),
                _ => Reply::Text("mixed child finished".into()),
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
    Native {
        name: &'static str,
        input: Value,
    },
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
            Self::Native { name, input } => json!({"tool":name,"input":input}),
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
            Self::Code(code) => {
                json!({"type":"custom_tool_call","name":"exec","call_id":id,"input":code})
            }
            Self::Text(text) => {
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
            }
            Self::Native { .. } | Self::Read { .. } | Self::Write { .. } => {
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
            Self::Native { name, input } => (
                json!({"type":"tool_use","id":id,"name":tool_name(name),"input":{}}),
                json!({"type":"input_json_delta","partial_json":input.to_string()}),
                "tool_use",
            ),
            Self::Read { path } => (
                json!({"type":"tool_use","id":id,"name":tool_name("Read"),"input":{}}),
                json!({"type":"input_json_delta","partial_json":json!({"file_path":path}).to_string()}),
                "tool_use",
            ),
            Self::Code(code) => (
                json!({"type":"tool_use","id":id,"name":tool_name("exec"),"input":{}}),
                json!({"type":"input_json_delta","partial_json":json!({"code":code}).to_string()}),
                "tool_use",
            ),
            Self::Text(text) => (
                json!({"type":"text","text":""}),
                json!({"type":"text_delta","text":text}),
                "end_turn",
            ),
            Self::Write { path, content } => (
                json!({"type":"tool_use","id":id,"name":tool_name("Write"),"input":{}}),
                json!({"type":"input_json_delta","partial_json":json!({"file_path":path,"content":content}).to_string()}),
                "tool_use",
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

fn contract() -> Value {
    json!({"kind":"object","fields":[{"name":"answer","required":true,"schema":{"kind":"string"}}]})
}
fn model(family: &str) -> &'static str {
    if family == "xai" {
        "grok-4.6"
    } else if family == "claude" {
        CLAUDE_MODEL
    } else {
        CODEX_MODEL
    }
}

// Routing uses user input only, never a tool catalog or fixture-script copy.
fn label(request: &Value) -> String {
    let input = request
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
        .join("\n");
    if input.contains("PAUSED_GRANDCHILD") {
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

fn native_receipt(request: &Value) -> Value {
    let value = last_tool_result(request);
    let text = if let Some(text) = value.as_str() {
        text.to_owned()
    } else {
        value
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    serde_json::from_str(&text).unwrap_or(json!(text))
}
fn native_error(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter_map(|m| m["content"].as_array())
        .flat_map(|b| b.iter().rev())
        .find(|b| b["type"] == "tool_result")
        .is_some_and(|b| b["is_error"] == true)
}

struct Servers {
    xai: String,
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
    let xai = format!("http://{}/v1/responses", http.local_addr()?);
    let state = Arc::clone(&provider);
    router = router.route("/v1/responses", post(move |headers: HeaderMap, Json(request): Json<Value>| {
        let state = Arc::clone(&state);
        async move {
            assert_eq!(headers["authorization"], "Bearer synthetic-xai-key");
            let label = label(&request);
            let reply = state.lock().unwrap().respond("xai", &label, request);
            let id = uuid::Uuid::new_v4().to_string();
            let output = match reply {
                Reply::Code(code) => json!({"type":"function_call","name":"exec","call_id":id,"arguments":json!({"code":code}).to_string()}),
                Reply::Text(text) => json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}),
                Reply::Read { path } => json!({"type":"function_call","name":"read_file","call_id":id,"arguments":json!({"target_file":path}).to_string()}),
                Reply::Write { path, content } => json!({"type":"function_call","name":"write","call_id":id,"arguments":json!({"file_path":path,"content":content}).to_string()}),
                Reply::Native { .. } => unreachable!("Claude native tools cannot route to xAI"),
                Reply::Pause => panic!("mixed xAI smoke journey must not pause"),
            };
            let event = json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[output],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
            ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
        }
    }));
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
                    if session_label.is_none() || current == "followup" {
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
        xai,
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
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
            "expected":format!("TUI displays {answer}, then exits successfully after Ctrl+D"),
            "boundary":"shipped nanocodex executable in a 140x32 PTY; only external providers use loopback fixtures",
            "input_after_answer":"Ctrl+D (0x04)",
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
        keyboard.write_all(b"\x04")?;
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
        kind == Journey::Mixed || family == "claude",
        kind == Journey::Smoke,
    );
    command.args(["--model", model(family)]);
    if kind == Journey::Smoke {
        command.arg("--prompt");
    }
    command.arg("HARNESS_ROUTING_ROOT");
    let answer = match kind {
        Journey::Smoke => "claude-only-answer",
        Journey::Mixed => "mixed-routing-answer",
        Journey::MissingChildAuth => "auth-denied-answer",
        Journey::Subscription => "subscription-native-answer",
        Journey::SubscriptionRecovery => "subscription-recovery-answer",
        Journey::XaiMixed => "xai-mixed-root-answer",
        Journey::ProjectContext => "project-context-answer",
        Journey::ContextRouting => "context-routing-answer",
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
    } else if kind == Journey::MissingChildAuth {
        assert_eq!(
            provider.log.len(),
            if family == "claude" { 3 } else { 2 },
            "unauthorized child contacted a model provider"
        );
        assert!(
            provider
                .connections
                .iter()
                .all(|connection| connection["family"] == family),
            "unauthorized child opened a provider connection"
        );
        if family != "claude" {
            assert!(
                provider.log[1]["tool_result"]
                    .to_string()
                    .contains("auth-denied-ok")
            );
        }
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
            // Claude receipts are asserted at each native call in native_script.
            if (label == "root" && family == "claude") || (label == "child" && family == "codex") {
                continue;
            }
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
        let effort = if family == "claude" {
            &inherited["request"]["output_config"]["effort"]
        } else {
            &inherited["request"]["reasoning"]["effort"]
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
async fn codex_claude_codex_nested_lifecycle_and_inheritance() -> Result<()> {
    journey("codex", Journey::Mixed).await
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
            vec!["--model", CLAUDE_MODEL],
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
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
    let mut invalid = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
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
                tools.iter().any(|tool| tool["name"] == "_Agent"),
                "native agent tool lacks subscription wire prefix"
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

fn xai_command(workspace: &Path, endpoint: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    command
        .arg("run")
        .current_dir(workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("CODEX_HOME", workspace.join("codex-home"))
        .env("NANOCODEX_COMPUTER", "off")
        .env("XAI_API_KEY", "synthetic-xai-key")
        .args([
            "--harness",
            "xai",
            "--xai-responses-url",
            endpoint,
            "--mcp-defaults",
            "false",
            "--mcp-codex-config",
            "false",
        ])
        .arg("--cwd")
        .arg(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn xai_cli_native_tools_children_and_durable_replay() -> Result<()> {
    let artifact = artifact("xai-native-tools-replay")?;
    let workspace = artifact.join("workspace");
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let counts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1/responses", listener.local_addr()?);
    let handler = {
        let requests = Arc::clone(&requests);
        let counts = Arc::clone(&counts);
        let artifact = artifact.clone();
        move |headers: HeaderMap, Json(request): Json<Value>| {
            let requests = Arc::clone(&requests);
            let counts = Arc::clone(&counts);
            let artifact = artifact.clone();
            async move {
                assert_eq!(headers["authorization"], "Bearer synthetic-xai-key");
                assert_eq!(request["model"], "grok-4.6");
                let who = label(&request);
                let stage = {
                    let mut counts = counts.lock().unwrap();
                    let n = counts.entry(who.clone()).or_default();
                    let stage = *n;
                    *n += 1;
                    stage
                };
                let id = format!("xai-{who}-{stage}");
                let call = |name: &str, arguments: Value| json!({"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string()});
                let code = |code: String| call("exec", json!({"code":code}));
                let output = if who == "inherited" {
                    match stage {
                        0 => code("text(await tools.submit_result({output:{answer:'xai-child-answer'}}));".into()),
                        _ => json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"child finished"}]}),
                    }
                } else {
                    match stage {
                        0 => {
                            let names: Vec<_> = request["tools"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .filter_map(|t| t["name"].as_str())
                                .collect();
                            for name in [
                                "write",
                                "read_file",
                                "search_replace",
                                "list_dir",
                                "glob",
                                "grep",
                                "run_terminal_cmd",
                                "exec",
                                "wait",
                                "tool_search",
                            ] {
                                assert!(names.contains(&name), "missing native CLI tool {name}");
                            }
                            call(
                                "write",
                                json!({"file_path":"xai-native.txt","content":"native-file-effect"}),
                            )
                        }
                        1 => call(
                            "run_terminal_cmd",
                            json!({"command":"cat xai-native.txt > xai-shell.txt; printf shell-ok", "description":"Copy the synthetic journey file"}),
                        ),
                        2 => {
                            assert!(last_tool_result(&request).to_string().contains("shell-ok"));
                            call("read_file", json!({"target_file":"xai-shell.txt"}))
                        }
                        3 => {
                            assert!(
                                last_tool_result(&request)
                                    .to_string()
                                    .contains("native-file-effect")
                            );
                            code(format!(
                                r#"
const child=await tools.spawn_agent({{harness:null,model:null,role:'INHERITED_CHILD',task:'INHERITED_CHILD',thinking:null,output_contract:{}}});
const done=await tools.wait_agent({{agent_ids:[child.agent_id],timeout_ms:20000}});text(done);
if(done.timed_out||done.agents[0].status.output.answer!=='xai-child-answer') throw Error('xAI child failed');
text('xai-child-ok');
"#,
                                contract()
                            ))
                        }
                        4 => {
                            assert!(
                                last_tool_result(&request)
                                    .to_string()
                                    .contains("xai-child-ok")
                            );
                            code(format!(
                                r#"
for(const family of ['claude','codex']) {{
 let denied=false;try {{await tools.spawn_agent({{harness:family,model:null,role:'AUTH_CHILD',task:'AUTH_CHILD',thinking:null,output_contract:{}}});}}
 catch(e) {{denied=true;text(String(e));}}
 if(!denied) throw Error('missing family auth was admitted');
}}
const directory=await tools.list_agents({{include_completed:true}});text(directory);
if(directory.agents.length!==1) throw Error('failed auth leaked a child');text('lazy-family-auth-ok');
"#,
                                contract()
                            ))
                        }
                        _ => {
                            assert!(
                                last_tool_result(&request)
                                    .to_string()
                                    .contains("lazy-family-auth-ok")
                            );
                            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"xai-native-cli-answer"}]})
                        }
                    }
                };
                {
                    let mut requests = requests.lock().unwrap();
                    requests
                        .push(json!({"label":who,"stage":stage,"request":request,"output":output}));
                    std::fs::write(
                        artifact.join("provider.json"),
                        serde_json::to_vec_pretty(&*requests).unwrap(),
                    )
                    .unwrap();
                }
                let event = json!({"type":"response.completed","response":{"id":id,"status":"completed","output":[output],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
                (
                    [("content-type", "text/event-stream")],
                    format!("data: {event}\n\n"),
                )
            }
        }
    };
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/responses", post(handler)),
        )
        .await
        .unwrap();
    });
    let database = workspace.join("native-xai.sqlite");
    let invocation = || {
        let mut command = xai_command(&workspace, &endpoint);
        command
            .args(["--rollouts", "false", "--local-durability"])
            .arg(&database)
            .args([
                "--local-durability-state-id",
                "xai-cli-root",
                "--request-id",
                "xai-cli-operation",
                "XAI_NATIVE_ROOT",
            ]);
        command
    };
    let initial = artifact.join("initial");
    std::fs::create_dir_all(&initial)?;
    let output = run(invocation(), &initial, "native xAI file/shell effects, same-family child, lazy missing family auth and final output").await?;
    success(&output, &initial, "xai-native-cli-answer")?;
    assert_eq!(
        std::fs::read_to_string(workspace.join("xai-native.txt"))?,
        "native-file-effect"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("xai-shell.txt"))?,
        "native-file-effect"
    );
    assert!(database.exists());
    let initial_requests = requests.lock().unwrap().len();
    assert!(initial_requests >= 7);
    let replay = artifact.join("replay");
    std::fs::create_dir_all(&replay)?;
    let output = run(
        invocation(),
        &replay,
        "terminal replay from SQLite, zero new provider calls or shell effects",
    )
    .await?;
    success(&output, &replay, "xai-native-cli-answer")?;
    assert_eq!(requests.lock().unwrap().len(), initial_requests);
    let events: Vec<Value> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["type"], "run.started");
    assert_eq!(events[1]["type"], "assistant.message");
    assert_eq!(events[2]["type"], "run.completed");
    assert_eq!(events[2]["payload"]["replayed"], true);
    assert_eq!(events[2]["payload"]["model_calls"], 0);
    task.abort();
    Ok(())
}

#[tokio::test]
async fn xai_cli_rejects_wrong_family_and_unsupported_options_without_dispatch() -> Result<()> {
    let artifact = artifact("xai-cli-invalid-options")?;
    let workspace = artifact.join("workspace");
    let cases: &[(&[&str], &str)] = &[
        (&["--model", "claude-sonnet-5-5"], "does not belong"),
        (
            &["--model", "grok-4.5", "--web-search", "true"],
            "does not support --web-search true",
        ),
        (&["--claude"], "conflicts"),
        (&["--thinking", "none"], "does not support thinking"),
        (&["--memory", "true"], "does not yet support --memory"),
        (
            &["--fast-mode", "true"],
            "--fast-mode true is not supported",
        ),
        (
            &["--image-generation", "true"],
            "--image-generation true is not supported",
        ),
        (
            &["--responses-transport", "websocket"],
            "uses HTTPS Responses SSE",
        ),
        (
            &["--store-responses", "true"],
            "--store-responses true is unsupported",
        ),
        (
            &["--reasoning-mode", "pro"],
            "--reasoning-mode is a Codex option",
        ),
    ];
    for (index, (args, expected)) in cases.iter().enumerate() {
        let path = artifact.join(index.to_string());
        std::fs::create_dir_all(&path)?;
        let mut command = xai_command(&workspace, "http://127.0.0.1:1/v1/responses");
        command.args(*args).arg("INVALID_XAI_ROOT");
        let output = run(command, &path, expected).await?;
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "unexpected CLI error: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "invalid options emitted model events"
        );
    }
    let path = artifact.join("missing-key");
    std::fs::create_dir_all(&path)?;
    let mut command = xai_command(&workspace, "http://127.0.0.1:1/v1/responses");
    command.env_remove("XAI_API_KEY").arg("MISSING_XAI_KEY");
    let output = run(
        command,
        &path,
        "missing xAI credential rejected before provider dispatch",
    )
    .await?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("XAI_API_KEY"));
    assert!(!workspace.join("codex-home/xai/sessions.sqlite").exists());
    Ok(())
}

#[tokio::test]
async fn xai_cli_mixed_children_route_back_to_codex_and_claude() -> Result<()> {
    for family in ["codex", "claude"] {
        let artifact = artifact(&format!("{family}-xai-{family}"))?;
        let provider = Arc::new(Mutex::new(Provider {
            root: family,
            journey: Journey::XaiMixed,
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
            true,
            true,
            false,
        );
        command.args([
            "--model",
            model(family),
            "--xai-api-key",
            "synthetic-xai-key",
            "--xai-responses-url",
            &servers.xai,
            "XAI_MIXED_ROOT",
        ]);
        let output = run(command, &artifact, "root -> xAI child -> original family grandchild, native transports and shared result contracts").await?;
        success(&output, &artifact, "xai-mixed-root-answer")?;
        let provider = provider.lock().unwrap();
        for (label, expected_family) in [("root", family), ("child", "xai"), ("grandchild", family)]
        {
            let calls: Vec<_> = provider
                .log
                .iter()
                .filter(|call| call["label"] == label)
                .collect();
            assert!(
                !calls.is_empty(),
                "{label} missing, inspect {}",
                artifact.display()
            );
            for call in calls {
                assert_eq!(call["family"], expected_family);
                assert_eq!(call["model"], model(expected_family));
            }
        }
        for label in ["root", "child"] {
            if family == "claude" && label == "root" {
                continue;
            } // Native result asserted in native_script.
            let call = provider
                .log
                .iter()
                .find(|call| call["label"] == label && call["stage"] == 1)
                .unwrap();
            assert!(
                call["tool_result"].to_string().contains("mixed-xai-ok"),
                "failed mixed result: {}",
                call["tool_result"]
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn xai_cli_model_defaults_select_supported_hosted_search() -> Result<()> {
    let artifact = artifact("xai-model-defaults")?;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1/responses", listener.local_addr()?);
    let app = Router::new().route("/v1/responses", post(move |Json(body): Json<Value>| {
        let captured = captured.clone();
        async move {
            captured.lock().unwrap().push(body);
            let event = json!({"type":"response.completed","response":{"id":"models","status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"model-defaults-ok"}]}]}});
            ([("content-type", "text/event-stream")], format!("data: {event}\n\n"))
        }
    }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for model in ["grok-4.5", "grok-4.6"] {
        let path = artifact.join(model);
        std::fs::create_dir_all(&path)?;
        let mut command = xai_command(&artifact.join("workspace"), &endpoint);
        command.args([
            "--model",
            model,
            "--rollouts",
            "false",
            "--subagents",
            "false",
            "MODEL_DEFAULTS",
        ]);
        let output = run(command, &path, "known model defaults admit a native Responses request with only supported hosted search").await?;
        success(&output, &path, "model-defaults-ok")?;
    }
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, model) in requests.iter().zip(["grok-4.5", "grok-4.6"]) {
        assert_eq!(request["model"], model);
        assert_eq!(
            request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["type"] == "web_search"),
            model == "grok-4.6"
        );
    }
    std::fs::write(
        artifact.join("provider.json"),
        serde_json::to_vec_pretty(&*requests)?,
    )?;
    task.abort();
    Ok(())
}

// Extract transmitted instruction data, independently of provider framing.
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
    if family == "xai" {
        let mut invocation = xai_command(workspace, &servers.xai);
        invocation.args(["--rollouts", "false", "--web-search", "false"]);
        invocation
    } else {
        command(workspace, servers, family, true, true, false)
    }
}

#[tokio::test]
async fn native_cli_project_context_is_bounded_lazy_and_explicitly_replaceable() -> Result<()> {
    for family in ["claude", "xai"] {
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
            assert!(names.contains(&if family == "claude" {
                "Read"
            } else {
                "read_file"
            }));
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
    for family in ["claude", "xai"] {
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
    for family in ["claude", "xai", "codex"] {
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
            if family == "xai" {
                invocation.args([
                    "--api-key",
                    "synthetic-openai-key",
                    "--claude-api-key",
                    "synthetic-anthropic-key",
                    "--claude-messages-url",
                    &servers.claude,
                    "--websocket-url",
                    &servers.codex,
                ]);
            } else {
                invocation.args([
                    "--xai-api-key",
                    "synthetic-xai-key",
                    "--xai-responses-url",
                    &servers.xai,
                ]);
            }
            if custom {
                invocation.args(["--instructions", "fixture-cross-family-override"]);
            }
            invocation.arg("CONTEXT_ROUTING_ROOT");
            let output = run(invocation, &artifact, "native root spawns Claude, xAI and Codex children; family defaults independent of parent, explicit replacement inherited without context").await?;
            success(&output, &artifact, "context-routing-answer")?;
            let provider = provider.lock().unwrap();
            if family != "claude" {
                assert!(
                    provider.log.last().unwrap()["tool_result"]
                        .to_string()
                        .contains("context-routing-ok")
                );
            }
            for target in ["claude", "xai", "codex"] {
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
