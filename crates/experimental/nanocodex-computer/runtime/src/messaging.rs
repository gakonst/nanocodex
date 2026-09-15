//! Explicitly configured messaging broker, with reviewed immutable send plans.
//! No account discovery, credential lookup, or transport configuration from JS.
use crate::{Error, Result, platforms::process};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
const MAX_ATTACHMENTS: usize = 5 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
struct Attachment {
    name: String,
    path: String,
    sha256: String,
    size: usize,
}
#[derive(Clone, Serialize, Deserialize)]
struct Plan {
    id: String,
    digest: String,
    account: String,
    recipients: Vec<String>,
    body: String,
    attachments: Vec<Attachment>,
    created_ms: u64,
    expires_ms: u64,
    state: String,
    reviewer: Option<String>,
    receipt: Value,
}
struct Broker {
    executable: PathBuf,
    args: Vec<String>,
    attachment_root: PathBuf,
    timeout: Duration,
}
#[derive(Default)]
pub struct Messaging {
    broker: Option<Broker>,
    plans: BTreeMap<String, Plan>,
    sequence: u64,
}
impl Messaging {
    pub fn new() -> Self {
        Self::default()
    }
    /// Host-only configuration. The configured executable is trusted to access the
    /// intended account; it is never selected by a document or message payload.
    pub fn configure(
        &mut self,
        executable: PathBuf,
        args: Vec<String>,
        attachment_root: PathBuf,
    ) -> Result<()> {
        if self.broker.is_some() {
            return Err(Error::action("Messaging broker already configured"));
        }
        if !executable.is_absolute() || !executable.is_file() {
            return Err(Error::invalid(
                "Broker executable must be an existing absolute file",
            ));
        }
        let root = fs::canonicalize(&attachment_root)?;
        if !root.is_dir() {
            return Err(Error::invalid(
                "Attachment root must be an existing directory",
            ));
        }
        self.broker = Some(Broker {
            executable,
            args,
            attachment_root: root,
            timeout: Duration::from_secs(15),
        });
        Ok(())
    }
    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let broker = self.broker.as_ref().ok_or_else(|| {
            Error::unsupported("Messaging requires explicit host broker configuration")
        })?;
        let output = process::run(
            &broker.executable,
            &broker.args,
            &serde_json::to_vec(
                &json!({"protocol":"skyre-messages-1","method":method,"params":params}),
            )?,
            broker.timeout,
        )
        .map_err(|e| Error::new(e.code, "Configured messaging broker failed"))?;
        let value: Value = serde_json::from_slice(&output)?;
        if value.get("ok") != Some(&Value::Bool(true)) {
            return Err(Error::action("Messaging broker rejected request"));
        }
        value
            .get("result")
            .cloned()
            .ok_or_else(|| Error::action("Messaging broker response missing result"))
    }
    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        if !params.is_object() {
            return Err(Error::invalid("Messaging params must be an object"));
        }
        match method{
   "messages.capabilities"=>Ok(json!({"configured":self.broker.is_some(),"protocol":"skyre-messages-1","methods":["messages.accounts","messages.list","messages.search","messages.read","messages.count","messages.attachment","messages.prepare","messages.commit","messages.status","messages.cancel","messages.reconcile"],"send_authorization":"host-issued review of immutable digest; no self-approval RPC"})),
   "messages.accounts"=>self.request("accounts",json!({})),
   "messages.list"|"messages.search"|"messages.count"=>{let account=required(params,"account")?;let limit=params.get("limit").map(|v|v.as_u64().ok_or_else(||Error::invalid("limit must be unsigned"))).transpose()?.unwrap_or(100);if !(1..=500).contains(&limit){return Err(Error::invalid("limit must be 1..500"));}let cursor=params.get("cursor").cloned().unwrap_or(Value::Null);if !cursor.is_null()&&!cursor.is_string(){return Err(Error::invalid("cursor must be a string"));}let query=params.get("query").and_then(Value::as_str).unwrap_or("");let result=self.request(method.trim_start_matches("messages."),json!({"account":account,"limit":limit,"cursor":cursor,"query":query}))?;if method!="messages.count"{let items=result.get("items").and_then(Value::as_array).ok_or_else(||Error::action("Broker listing missing items"))?;if items.len()>limit as usize{return Err(Error::action("Broker exceeded requested page size"));}
 if result.get("next_cursor").is_some_and(|v|!v.is_null()&&!v.is_string()){return Err(Error::action("Invalid broker next_cursor"));}}else if result.get("count").and_then(Value::as_u64).is_none(){return Err(Error::action("Broker count missing unsigned count"));}Ok(result)},
   "messages.read"=>self.request("read",json!({"account":required(params,"account")?,"message_id":required(params,"message_id")?})),
   "messages.attachment"=>{let result=self.request("attachment",json!({"account":required(params,"account")?,"message_id":required(params,"message_id")?,"attachment_id":required(params,"attachment_id")?}))?;let encoded=required(&result,"data")?;let bytes=STANDARD.decode(encoded).map_err(|_|Error::action("Broker attachment is not base64"))?;if bytes.len()>MAX_ATTACHMENTS{return Err(Error::action("Broker attachment exceeds 5MiB"));}let mime=required(&result,"mime_type")?;Ok(json!({"mime_type":mime,"data":encoded,"size":bytes.len(),"sha256":digest(&bytes)}))},
   "messages.prepare"=>self.prepare(params),
   "messages.commit"=>self.commit(required(params,"plan_id")?,required(params,"digest")?),
   "messages.status"=>{let plan=self.plans.get(required(params,"plan_id")?).ok_or_else(||Error::action("Unknown message plan"))?;Ok(serde_json::to_value(plan)?)},
   "messages.cancel"=>{let plan=self.plans.get_mut(required(params,"plan_id")?).ok_or_else(||Error::action("Unknown message plan"))?;if !["prepared","approved"].contains(&plan.state.as_str()){return Err(Error::action("Message plan cannot be cancelled in its current state"));}plan.state="cancelled".into();plan.reviewer=None;Ok(json!({"cancelled":true}))},
   "messages.reconcile"=>self.reconcile(required(params,"plan_id")?),
   _=>Err(Error::unsupported(format!("Unknown messaging method: {method}")))
  }
    }
    fn prepare(&mut self, params: &Value) -> Result<Value> {
        if self.plans.len() >= 128 {
            return Err(Error::action("Prepared message limit reached"));
        }
        let account = required(params, "account")?.to_string();
        let recipients: Vec<String> = serde_json::from_value(
            params
                .get("recipients")
                .cloned()
                .ok_or_else(|| Error::invalid("recipients required"))?,
        )
        .map_err(|_| Error::invalid("recipients must be strings"))?;
        if recipients.is_empty()
            || recipients.len() > 100
            || recipients
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 4096)
        {
            return Err(Error::invalid("Invalid recipients"));
        }
        let body = params
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::invalid("body must be a string"))?
            .to_string();
        if body.len() > 256 * 1024 {
            return Err(Error::invalid("Message body exceeds 256KiB"));
        }
        let paths: Vec<String> = params
            .get("attachments")
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()?
            .unwrap_or_default();
        if paths.len() > 20 {
            return Err(Error::invalid("At most 20 attachments"));
        }
        let mut total = 0;
        let mut attachments = vec![];
        for path in paths {
            let bytes = self.read_attachment(&path)?;
            total += bytes.len();
            if total > MAX_ATTACHMENTS {
                return Err(Error::invalid("Attachments exceed 5MiB total"));
            }
            attachments.push(Attachment {
                name: Path::new(&path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                path,
                sha256: digest(&bytes),
                size: bytes.len(),
            });
        }
        if body.is_empty() && attachments.is_empty() {
            return Err(Error::invalid("Message must contain body or attachments"));
        }
        // Resolution is read-only. The broker returns canonical addresses for review.
        let resolution = self.request(
            "resolve_recipients",
            json!({"account":account,"recipients":recipients}),
        )?;
        if resolution.get("account").and_then(Value::as_str) != Some(&account) {
            return Err(Error::action("Broker resolved a different account"));
        }
        let canonical: Vec<String> = serde_json::from_value(
            resolution
                .get("recipients")
                .cloned()
                .ok_or_else(|| Error::action("Resolved recipients missing"))?,
        )?;
        if canonical.len() != recipients.len()
            || canonical
                .iter()
                .any(|s| s.trim().is_empty() || s.len() > 4096)
        {
            return Err(Error::action("Invalid resolved recipients"));
        }
        let ttl = params
            .get("expires_in_ms")
            .map(|v| {
                v.as_u64()
                    .ok_or_else(|| Error::invalid("expires_in_ms must be unsigned"))
            })
            .transpose()?
            .unwrap_or(300000);
        if !(1..=300000).contains(&ttl) {
            return Err(Error::invalid("expires_in_ms must be 1..300000"));
        }
        self.sequence += 1;
        let created = now();
        let id = format!("send-{}-{created}-{}", std::process::id(), self.sequence);
        let mut plan = Plan {
            id: id.clone(),
            digest: String::new(),
            account,
            recipients: canonical,
            body,
            attachments,
            created_ms: created,
            expires_ms: created + ttl,
            state: "prepared".into(),
            reviewer: None,
            receipt: Value::Null,
        };
        plan.digest = plan_digest(&plan)?;
        let value = serde_json::to_value(&plan)?;
        self.plans.insert(id, plan);
        Ok(value)
    }
    /// Only the trusted host calls this after reviewing the concrete returned plan.
    /// The RPC facade deliberately has no authorize/approve method.
    pub fn authorize(
        &mut self,
        plan_id: &str,
        expected_digest: &str,
        reviewer: &str,
    ) -> Result<()> {
        if !["user", "guardian_subagent", "auto_review"].contains(&reviewer) {
            return Err(Error::invalid("Unknown explicit reviewer"));
        }
        let plan = self
            .plans
            .get_mut(plan_id)
            .ok_or_else(|| Error::action("Unknown message plan"))?;
        validate_plan(plan, expected_digest)?;
        if plan.state != "prepared" {
            return Err(Error::action("Message plan is not awaiting review"));
        }
        plan.state = "approved".into();
        plan.reviewer = Some(reviewer.into());
        Ok(())
    }
    fn commit(&mut self, plan_id: &str, expected_digest: &str) -> Result<Value> {
        let plan = self
            .plans
            .get(plan_id)
            .ok_or_else(|| Error::action("Unknown message plan"))?
            .clone();
        validate_plan(&plan, expected_digest)?;
        if plan.state != "approved" || plan.reviewer.is_none() {
            return Err(Error::new(
                -32003,
                "Message requires explicit host review before commit",
            ));
        }
        let mut attachments = vec![];
        for attachment in &plan.attachments {
            let bytes = self.read_attachment(&attachment.path)?;
            if bytes.len() != attachment.size || digest(&bytes) != attachment.sha256 {
                return Err(Error::action("Attachment changed after review"));
            }
            attachments.push(json!({"name":attachment.name,"sha256":attachment.sha256,"data":STANDARD.encode(bytes)}));
        }
        // Revalidate resolved identities immediately before dispatch, without silently
        // accepting a redirect to a different account or recipient.
        let resolution = self.request(
            "resolve_recipients",
            json!({"account":plan.account,"recipients":plan.recipients}),
        )?;
        if resolution.get("account") != Some(&json!(plan.account))
            || resolution.get("recipients") != Some(&json!(plan.recipients))
        {
            return Err(Error::action("Account or recipients changed after review"));
        }
        validate_plan(&plan, expected_digest)?;
        self.plans.get_mut(plan_id).unwrap().state = "committing".into();
        let result=self.request("commit_send",json!({"idempotency_key":plan.id,"digest":plan.digest,"account":plan.account,"recipients":plan.recipients,"body":plan.body,"attachments":attachments,"reviewer":plan.reviewer}));
        let stored = self.plans.get_mut(plan_id).unwrap();
        stored.reviewer = None;
        match result {
            Ok(receipt) => {
                stored.state = "sent".into();
                stored.receipt = receipt.clone();
                Ok(json!({"state":"sent","receipt":receipt}))
            }
            Err(error) => {
                stored.state = "uncertain".into();
                Err(Error::new(
                    error.code,
                    format!(
                        "Send outcome is uncertain; do not retry automatically: {}",
                        error.message
                    ),
                ))
            }
        }
    }
    fn reconcile(&mut self, id: &str) -> Result<Value> {
        let plan = self
            .plans
            .get(id)
            .ok_or_else(|| Error::action("Unknown message plan"))?
            .clone();
        if plan.state != "uncertain" {
            return Err(Error::action("Only uncertain sends can be reconciled"));
        }
        let status = self.request(
            "send_status",
            json!({"account":plan.account,"idempotency_key":plan.id,"digest":plan.digest}),
        )?;
        let outcome = required(&status, "state")?;
        let plan = self.plans.get_mut(id).unwrap();
        match outcome {
            "sent" => {
                plan.state = "sent".into();
                plan.receipt = status.clone();
            }
            "not_sent" => {
                plan.state = "failed".into();
            }
            "unknown" => (),
            _ => return Err(Error::action("Invalid send status")),
        };
        Ok(json!({"state":plan.state,"broker":status}))
    }
    fn read_attachment(&self, path: &str) -> Result<Vec<u8>> {
        let broker = self
            .broker
            .as_ref()
            .ok_or_else(|| Error::unsupported("Messaging broker not configured"))?;
        if path.is_empty()
            || path.contains('\\')
            || path.contains(':')
            || Path::new(path)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(Error::invalid(
                "Attachment paths must be confined relative paths",
            ));
        }
        let mut current = broker.attachment_root.clone();
        for component in Path::new(path).components() {
            current.push(component);
            if fs::symlink_metadata(&current)?.file_type().is_symlink() {
                return Err(Error::invalid("Attachment paths cannot contain symlinks"));
            }
        }
        let canonical = fs::canonicalize(&current)?;
        if !canonical.starts_with(&broker.attachment_root) || !canonical.is_file() {
            return Err(Error::invalid("Attachment escaped configured root"));
        }
        let mut bytes = vec![];
        fs::File::open(canonical)?
            .take(MAX_ATTACHMENTS as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_ATTACHMENTS {
            return Err(Error::invalid("Attachment exceeds 5MiB"));
        }
        Ok(bytes)
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::invalid(format!("{key} must be a nonempty string")))
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn plan_digest(plan: &Plan) -> Result<String> {
    Ok(digest(&serde_json::to_vec(
        &json!({"id":plan.id,"account":plan.account,"recipients":plan.recipients,"body":plan.body,"attachments":plan.attachments,"created_ms":plan.created_ms,"expires_ms":plan.expires_ms}),
    )?))
}
fn validate_plan(plan: &Plan, expected_digest: &str) -> Result<()> {
    if now() >= plan.expires_ms {
        return Err(Error::action(
            "Message plan expired; prepare and review again",
        ));
    }
    if expected_digest != plan.digest || plan_digest(plan)? != expected_digest {
        return Err(Error::new(-32003, "Message plan digest mismatch"));
    }
    Ok(())
}
