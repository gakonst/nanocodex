//! Local CUA approval state. Only a trusted host response can create a grant.
use crate::{Error, Result, native::App, security::Security};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const APP_METHODS: &[&str] = &[
    "get_app_state",
    "click",
    "drag",
    "paste",
    "perform_secondary_action",
    "press_key",
    "scroll",
    "select_text",
    "set_value",
    "type_text",
];

#[derive(Default)]
pub struct Approvals {
    targets: BTreeMap<String, App>,
    apps: BTreeSet<String>,
    audio: bool,
}
impl Approvals {
    pub fn policy(&mut self, app: App, security: &Security) -> Value {
        let allowed = [app.id.as_str(), app.path.as_str(), app.name.as_str()]
            .iter()
            .any(|id| security.check_app(id).is_ok());
        let policy = json!({"decision":if allowed {"allowed"} else {"denied"},
            "allowPersistentApproval":true,"target":{"bundleIdentifier":app.id,
            "displayName":app.name,"appPath":app.path,"risk":"high"}});
        self.targets.insert(app.id.clone(), app);
        policy
    }
    pub fn prepare(
        &mut self,
        request: &Value,
        security: &Security,
        synthetic: bool,
    ) -> Result<(Value, Option<Value>)> {
        self.prepare_for_platform(request, security, synthetic, "mac")
    }
    pub fn prepare_for_platform(
        &mut self,
        request: &Value,
        security: &Security,
        synthetic: bool,
        platform: &str,
    ) -> Result<(Value, Option<Value>)> {
        let meta = &request["meta"];
        if meta["connector_id"] != "computer-use" {
            return Err(Error::unsupported(
                "Only Computer Use elicitations are supported",
            ));
        }
        let windows = platform == "windows";
        let method = meta["tool_name"]
            .as_str()
            .or(if windows {
                Some("get_window_state")
            } else {
                None
            })
            .ok_or_else(|| Error::invalid("Elicitation tool_name is required"))?;
        let (message, mut canonical_meta, preapproved) = if method == "start_audio_recording" {
            (
                "Allow Computer Use to record computer audio?".to_string(),
                json!({"codex_request_type":"approval_request","persist":["session"],
                "riskLevel":"high","tool_params":if windows {json!({"app":"computer-audio"})} else {json!({})},
                "tool_params_display":if windows {json!([{"name":"app","display_name":"App","value":"Computer audio"}])} else {json!([])}}),
                security.audio_preapproved(),
            )
        } else {
            if !APP_METHODS.contains(&method) && !(windows && method == "get_window_state") {
                return Err(Error::invalid("Unknown Computer Use approval method"));
            }
            let app = self
                .targets
                .get(meta["tool_params"]["app"].as_str().unwrap_or_default())
                .ok_or_else(|| {
                    Error::invalid("Resolve an app policy before requesting approval")
                })?;
            if ![app.id.as_str(), app.path.as_str(), app.name.as_str()]
                .iter()
                .any(|id| security.check_app(id).is_ok())
            {
                return Err(Error::new(
                    -32010,
                    "Application denied by configured policy",
                ));
            }
            (
                if windows {
                    format!("Allow Codex to use {}?", app.name)
                } else {
                    format!("Allow Computer Use to use \"{}\"?", app.name)
                },
                json!({"persist":["session","always"],"riskLevel":"high",
                "tool_params":{"app":app.id},
                "tool_params_display":[{"name":"app","display_name":"App","value":app.name}]}),
                synthetic || security.app_preapproved(app) || self.apps.contains(&app.path),
            )
        };
        let output = canonical_meta.as_object_mut().unwrap();
        output.insert("codex_approval_kind".into(), json!("mcp_tool_call"));
        output.insert("connector_id".into(), json!("computer-use"));
        output.insert("connector_name".into(), json!("Computer Use"));
        if !windows || method == "start_audio_recording" {
            output.insert("tool_name".into(), json!(method));
        }
        if (!windows || method == "start_audio_recording")
            && let Some(id) = meta["tool_call_id"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
        {
            output.insert("tool_call_id".into(), json!(id.trim()));
        }
        let canonical = json!({"message":message,"meta":canonical_meta});
        let response = if preapproved {
            Some(self.resolve(
                &canonical,
                &json!({"action":"accept","content":{"source":"computer-use-persisted-state"}}),
            )?)
        } else {
            None
        };
        Ok((canonical, response))
    }
    /// Called by the host with the exact request it sent and the client response.
    /// There is deliberately no corresponding public JSON-RPC grant method.
    pub fn resolve(&mut self, request: &Value, response: &Value) -> Result<Value> {
        let action = response["action"]
            .as_str()
            .ok_or_else(|| Error::invalid("Elicitation response action is required"))?;
        if !["accept", "decline", "cancel"].contains(&action) {
            return Err(Error::invalid("Invalid elicitation response action"));
        }
        let meta = &request["meta"];
        if meta["tool_name"] == "start_audio_recording" {
            if meta["tool_params"]["app"] == "computer-audio"
                && action == "accept"
                && (response["_meta"]["persist"] == "always"
                    || response["content"]["persist"] == "always"
                    || (response["content"]["source"] == "computer-use-persisted-state"
                        && response["content"]["scope"] == "global"))
            {
                return Err(Error::new(
                    -32003,
                    "Computer audio approval cannot persist globally",
                ));
            }
            self.audio = action == "accept";
        } else {
            let app = self
                .targets
                .get(meta["tool_params"]["app"].as_str().unwrap_or_default())
                .ok_or_else(|| Error::invalid("Approval target is no longer available"))?;
            if action == "accept" {
                self.apps.insert(app.path.clone());
            } else {
                self.apps.remove(&app.path);
            }
        }
        Ok(response.clone())
    }
    pub fn authorize_app(&self, identifier: &str) -> Result<()> {
        if self.apps.contains(identifier) {
            Ok(())
        } else {
            Err(Error::new(
                -32003,
                "Computer Use requires host approval for this app",
            ))
        }
    }
    pub fn consume_audio(&mut self) -> Result<()> {
        if std::mem::take(&mut self.audio) {
            Ok(())
        } else {
            Err(Error::new(
                -32003,
                "Computer Use requires host approval to record computer audio",
            ))
        }
    }
    pub fn clear(&mut self) {
        self.apps.clear();
        self.audio = false;
        self.targets.clear();
    }
}
