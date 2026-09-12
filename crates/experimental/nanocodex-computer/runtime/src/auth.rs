//! Local broker handoff with document-bound checkpoints. Credential values are
//! never accepted through public tool params or returned in the tool result.
use crate::{
    Error, Result,
    browser::Browsers,
    qr,
    security::{Document, Security, origin},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroize;
pub trait Page {
    fn execute(&mut self, method: &str, args: &Value) -> Result<Value>;
}
impl Page for Browsers {
    fn execute(&mut self, method: &str, args: &Value) -> Result<Value> {
        Browsers::execute(self, method, args)
    }
}
fn yes() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    pub id: String,
    pub selector: String,
    #[serde(default)]
    pub label: String,
    #[serde(default = "yes")]
    pub required: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OptionChoice {
    pub id: String,
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub label: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Submit {
    pub selector: String,
    #[serde(default)]
    pub press_enter: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub browser: String,
    pub tab: String,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub options: Option<Vec<OptionChoice>>,
    #[serde(default)]
    pub submit: Option<Submit>,
    #[serde(default)]
    pub qr_code: bool,
}
#[derive(Clone)]
struct Challenge {
    request: Request,
    document: Document,
    metadata: Vec<Value>,
    controls: Vec<(String, Value)>,
    otp: bool,
    expires: std::time::Instant,
    status: String,
    watch: qr::Watch,
}
#[derive(Default)]
pub struct Auth {
    challenges: BTreeMap<String, Challenge>,
}
impl Auth {
    pub fn execute(
        &mut self,
        method: &str,
        args: &Value,
        browsers: &mut impl Page,
        security: &Security,
    ) -> Result<Value> {
        self.expire(security);
        match method {
            "begin" => self.begin(
                serde_json::from_value(args.clone())
                    .map_err(|_| Error::invalid("Invalid authentication request"))?,
                browsers,
                security,
            ),
            "poll" => {
                let id = crate::engine::string(args, "id")?;
                self.poll(id, browsers, security)
            }
            "cancel" => {
                let id = crate::engine::string(args, "id")?;
                let challenge = self
                    .challenges
                    .get_mut(id)
                    .ok_or_else(|| Error::invalid("Unknown challenge"))?;
                if challenge.status == "pending" {
                    challenge.status = "cancelled".into();
                    challenge.watch.stop();
                    Self::finish_broker(security, id, "cancelled");
                }
                Ok(json!({"status":challenge.status}))
            }
            "status" => {
                let id = crate::engine::string(args, "id")?;
                let c = self
                    .challenges
                    .get(id)
                    .ok_or_else(|| Error::invalid("Unknown challenge"))?;
                Ok(json!({"id":id,"status":c.status,"qrWatch":c.watch}))
            }
            "qr_decode" => {
                let encoded = crate::engine::string(args, "data")?;
                if encoded.len() > 12 * 1024 * 1024 {
                    return Err(Error::invalid("QR input too large"));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| Error::invalid("Invalid image encoding"))?;
                Ok(serde_json::to_value(qr::decode(&bytes)?)?)
            }
            _ => Err(Error::unsupported("Unknown authentication operation")),
        }
    }
    fn begin(
        &mut self,
        request: Request,
        browsers: &mut impl Page,
        security: &Security,
    ) -> Result<Value> {
        validate_request(&request)?;
        let broker = security.broker.as_ref().ok_or_else(|| {
            Error::new(
                -32011,
                "Authentication broker unavailable; configure an explicit local broker",
            )
        })?;
        if self
            .challenges
            .values()
            .filter(|c| c.status == "pending")
            .count()
            >= 16
        {
            return Err(Error::action("Too many authentication challenges"));
        }
        let document = context(browsers, &request)?;
        security.check_url(&document.url)?;
        let mut metadata = vec![];
        for field in &request.fields {
            let mut m = inspect(browsers, &document, &field.selector)?;
            validate_field(&m)?;
            m["id"] = json!(field.id);
            m["label"] = json!(normalize_label(m["label"].as_str().unwrap_or(&field.label)));
            metadata.push(m);
        }
        let otp = is_otp(&metadata);
        let mut controls = vec![];
        if let Some(options) = &request.options {
            for option in options {
                if otp && !option.fields.is_empty() && option.fields.len() != request.fields.len() {
                    return Err(Error::invalid("OTP options must select the entire group"));
                }
                if let Some(selector) = &option.selector {
                    let m = inspect(browsers, &document, selector)?;
                    validate_control(&m, false)?;
                    controls.push((selector.clone(), presentation(&m)));
                }
            }
        }
        if let Some(submit) = &request.submit {
            let m = inspect(browsers, &document, &submit.selector)?;
            validate_control(&m, !submit.press_enter)?;
            controls.push((submit.selector.clone(), presentation(&m)));
            if let Some(submission) = m["submissionOrigin"].as_str()
                && origin(submission)? != origin(&document.url)?
            {
                return Err(Error::new(
                    -32014,
                    "Cross-origin form submission requires an explicit provider binding",
                ));
            }
        }
        document.validate(&context(browsers, &request)?)?;
        let mut watch = qr::Watch::default();
        let qr = if request.qr_code {
            let image = browsers.execute(
                "screenshot",
                &json!({"browser":request.browser,"tab":request.tab}),
            )?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(
                    image["data"]
                        .as_str()
                        .ok_or_else(|| Error::action("Missing screenshot data"))?,
                )
                .map_err(|_| Error::action("Invalid screenshot data"))?;
            let result = qr::decode(&bytes)?;
            let symbol = watch
                .observe(result, false, request.fields.is_empty())?
                .ok_or_else(|| Error::action("No unique usable QR code"))?;
            Some(symbol)
        } else {
            None
        };
        let prompt = prompt(&request, &metadata, otp, qr.as_ref(), &document.url);
        // Review gets observed presentation and action names, never selectors or secrets.
        let dom = browsers.execute(
            "dom_snapshot",
            &json!({"browser":request.browser,"tab":request.tab}),
        )?;
        security.review(json!({"document":document,"visibleDom":dom,"prompt":prompt,"actions":{"fillFields":request.fields.iter().map(|f|&f.id).collect::<Vec<_>>(),"submit":request.submit.is_some()}}))?;
        document.validate(&context(browsers, &request)?)?;
        let id = token()?;
        let response=match broker.request(&json!({"type":"begin","id":id,"document":document,"prompt":prompt,"requested_schema":{"type":"object","properties":{},"additionalProperties":false}})){Ok(value)=>value,Err(error)=>{Self::finish_broker(security,&id,"unavailable");return Err(error);}};
        if response["status"] != "pending" {
            Self::finish_broker(security, &id, "unavailable");
            return Ok(json!({"status":"unavailable"}));
        }
        self.challenges.insert(
            id.clone(),
            Challenge {
                request,
                document,
                metadata,
                controls,
                otp,
                expires: std::time::Instant::now() + std::time::Duration::from_secs(300),
                status: "pending".into(),
                watch,
            },
        );
        Ok(
            json!({"id":id,"status":"pending","prompt":prompt,"requestedSchema":{"type":"object","properties":{},"additionalProperties":false}}),
        )
    }
    fn poll(&mut self, id: &str, browsers: &mut impl Page, security: &Security) -> Result<Value> {
        let challenge = self
            .challenges
            .get_mut(id)
            .ok_or_else(|| Error::invalid("Unknown challenge"))?;
        if challenge.status != "pending" {
            return Ok(json!({"id":id,"status":challenge.status}));
        }
        let broker = security
            .broker
            .as_ref()
            .ok_or_else(|| Error::action("Broker unavailable"))?;
        let outcome = (|| {
            if challenge.request.qr_code && poll_qr(challenge, id, browsers, security)? {
                return Ok("submitted".to_string());
            }
            let mut result = broker.request(&json!({"type":"poll","id":id}))?;
            let status = result["status"]
                .as_str()
                .unwrap_or("unavailable")
                .to_string();
            let outcome = match status.as_str() {
                "pending" => Ok("pending".to_string()),
                "declined" | "cancelled" => Ok(status),
                "submitted" => {
                    apply(challenge, &result, browsers, security).map(|_| "submitted".to_string())
                }
                _ => Ok("unavailable".into()),
            };
            wipe(&mut result);
            outcome
        })();
        match outcome {
            Ok(status) => {
                challenge.status = status.clone();
                if status != "pending" {
                    challenge.watch.stop();
                    Self::finish_broker(security, id, &status);
                }
                Ok(json!({"id":id,"status":status}))
            }
            Err(error) => {
                challenge.status = if error.code == -32014 {
                    "page_changed".into()
                } else {
                    "submission_failed".into()
                };
                challenge.watch.stop();
                Self::finish_broker(security, id, &challenge.status);
                Err(error)
            }
        }
    }
    fn finish_broker(security: &Security, id: &str, status: &str) {
        if let Some(broker) = &security.broker {
            let _ = broker.request(&json!({"type":"complete","id":id,"status":status}));
            let _ = broker.request(&json!({"type":"close","id":id}));
        }
    }
    fn expire(&mut self, security: &Security) {
        for (id, c) in &mut self.challenges {
            if c.status == "pending" && std::time::Instant::now() >= c.expires {
                c.status = "expired".into();
                c.watch.stop();
                Self::finish_broker(security, id, "expired");
            }
        }
        self.challenges.retain(|_, c| {
            c.status == "pending"
                || std::time::Instant::now() < c.expires + std::time::Duration::from_secs(300)
        });
    }
    pub fn shutdown(&mut self, security: &Security) {
        for (id, c) in &mut self.challenges {
            if c.status == "pending" {
                c.status = "cancelled".into();
                c.watch.stop();
                Self::finish_broker(security, id, "cancelled");
            }
        }
    }
}
fn context(b: &mut impl Page, r: &Request) -> Result<Document> {
    serde_json::from_value(b.execute(
        "document_context",
        &json!({"browser":r.browser,"tab":r.tab}),
    )?)
    .map_err(|_| Error::action("Browser did not provide a complete document identity"))
}
fn inspect(b: &mut impl Page, d: &Document, selector: &str) -> Result<Value> {
    let mut args = d.args();
    args["selector"] = json!(selector);
    b.execute("locator_inspect", &args)
}
fn validate_field(m: &Value) -> Result<()> {
    validate_control(m, false)?;
    if !m["nodeIdentity"].as_str().is_some_and(|id| !id.is_empty()) {
        return Err(Error::new(-32014, "Credential target identity unavailable"));
    }
    if m["editable"] != true {
        return Err(Error::invalid("Credential target is not editable"));
    }
    Ok(())
}
fn validate_control(m: &Value, allow_disabled: bool) -> Result<()> {
    if m["count"] != 1 || m["visible"] != true || (!allow_disabled && m["enabled"] != true) {
        return Err(Error::invalid(
            "Credential control must be unique, visible and enabled",
        ));
    }
    Ok(())
}
fn presentation(m: &Value) -> Value {
    json!({"tag":m["tag"],"type":m["type"],"label":normalize_label(m["label"].as_str().unwrap_or("")),"submissionOrigin":m["submissionOrigin"],"nodeIdentity":m["nodeIdentity"],"formIdentity":m["formIdentity"]})
}
fn poll_qr(c: &mut Challenge, id: &str, b: &mut impl Page, security: &Security) -> Result<bool> {
    let changed = if c.watch.needs_binding_check() {
        {
            let current = context(b, &c.request)?;
            security.check_url(&current.url)?;
            c.document.validate(&current).is_err()
        }
    } else {
        false
    };
    let captured = b.execute(
        "screenshot",
        &json!({"browser":c.request.browser,"tab":c.request.tab}),
    );
    let decoded = match captured {
        Ok(image) => image["data"]
            .as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .and_then(|bytes| qr::decode(&bytes).ok())
            .unwrap_or(qr::Decode::Failed),
        Err(_) => qr::Decode::Failed,
    };
    if let Some(symbol) = c
        .watch
        .observe(decoded, changed, c.request.fields.is_empty())?
        && let Some(broker) = &security.broker
    {
        broker.request(&json!({"type":"qr_update","id":id,"qr":symbol}))?;
    }
    Ok(c.watch.terminal)
}
fn checkpoint(c: &Challenge, b: &mut impl Page, security: &Security) -> Result<()> {
    if std::time::Instant::now() >= c.expires {
        return Err(Error::new(-32013, "Authentication challenge expired"));
    }
    let current = context(b, &c.request)?;
    c.document.validate(&current)?;
    security.check_url(&current.url)?;
    for (field, reviewed) in c.request.fields.iter().zip(&c.metadata) {
        let m = inspect(b, &c.document, &field.selector)?;
        validate_field(&m)?;
        for key in [
            "type",
            "autocomplete",
            "inputMode",
            "nodeIdentity",
            "formIdentity",
        ] {
            if m[key] != reviewed[key] {
                return Err(Error::new(-32014, "prompt_changed"));
            }
        }
        if normalize_label(m["label"].as_str().unwrap_or(&field.label))
            != reviewed["label"].as_str().unwrap_or("")
        {
            return Err(Error::new(-32014, "prompt_changed"));
        }
    }
    for (selector, reviewed) in &c.controls {
        let m = inspect(b, &c.document, selector)?;
        validate_control(&m, true)?;
        if presentation(&m) != *reviewed {
            return Err(Error::new(-32014, "prompt_changed"));
        }
    }
    c.document.validate(&context(b, &c.request)?)?;
    Ok(())
}
fn bound_control_args(c: &Challenge, selector: &str) -> Result<Value> {
    let reviewed = c
        .request
        .fields
        .iter()
        .position(|field| field.selector == selector)
        .and_then(|index| c.metadata.get(index))
        .or_else(|| {
            c.controls
                .iter()
                .find(|(value, _)| value == selector)
                .map(|(_, metadata)| metadata)
        })
        .ok_or_else(|| Error::new(-32014, "reviewed_control_unavailable"))?;
    if !reviewed["nodeIdentity"]
        .as_str()
        .is_some_and(|identity| !identity.is_empty())
    {
        return Err(Error::new(-32014, "reviewed_control_unavailable"));
    }
    let mut args = c.document.args();
    args["selector"] = json!(selector);
    args["expectedNodeIdentity"] = reviewed["nodeIdentity"].clone();
    args["expectedFormIdentity"] = reviewed["formIdentity"].clone();
    Ok(args)
}
fn apply(c: &Challenge, response: &Value, b: &mut impl Page, security: &Security) -> Result<()> {
    checkpoint(c, b, security)?;
    let selected = response["option"].as_str();
    let values = response["values"]
        .as_object()
        .ok_or_else(|| Error::invalid("Broker submission values must be an object"))?;
    let option = match (&c.request.options, selected) {
        (Some(options), Some(id)) => Some(
            options
                .iter()
                .find(|o| o.id == id)
                .ok_or_else(|| Error::invalid("Unknown selected option"))?,
        ),
        (Some(options), None) if !options.is_empty() => {
            return Err(Error::invalid("Option selection required"));
        }
        (None, Some(_)) => return Err(Error::invalid("Unexpected selected option")),
        _ => None,
    };
    let fields: Vec<_> = c
        .request
        .fields
        .iter()
        .filter(|f| option.is_none_or(|o| o.fields.contains(&f.id)))
        .collect();
    let allowed: BTreeSet<_> = if c.otp && !fields.is_empty() {
        BTreeSet::from(["otp"])
    } else {
        fields.iter().map(|f| f.id.as_str()).collect()
    };
    for (id, value) in values {
        if !allowed.contains(id.as_str())
            || !value
                .as_str()
                .is_some_and(|s| s.encode_utf16().count() <= 16384)
        {
            return Err(Error::invalid("Broker supplied invalid credential fields"));
        }
    }
    for f in &fields {
        let id = if c.otp { "otp" } else { f.id.as_str() };
        if f.required
            && !values
                .get(id)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        {
            return Err(Error::invalid("Required broker field missing"));
        }
    }
    if c.otp && !fields.is_empty() {
        let code = values
            .get("otp")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '-')
            .collect::<Vec<_>>();
        if code.len() != fields.len() {
            return Err(Error::invalid("OTP length does not match input count"));
        }
        for (f, ch) in fields.iter().zip(code) {
            checkpoint(c, b, security)?;
            let mut args = bound_control_args(c, &f.selector)?;
            args["value"] = json!(ch.to_string());
            let result = b.execute("locator_fill", &args);
            wipe(&mut args);
            result.map_err(sanitize_action_error)?;
        }
    } else {
        for f in fields {
            let Some(value) = values.get(&f.id).and_then(Value::as_str) else {
                continue;
            };
            checkpoint(c, b, security)?;
            let mut args = bound_control_args(c, &f.selector)?;
            args["value"] = json!(value);
            let result = b.execute("locator_fill", &args);
            wipe(&mut args);
            result.map_err(sanitize_action_error)?;
        }
    }
    if let Some(option) = option.filter(|o| o.fields.is_empty()) {
        if let Some(selector) = &option.selector {
            checkpoint(c, b, security)?;
            let args = bound_control_args(c, selector)?;
            b.execute("locator_click", &args)?;
        }
        return Ok(());
    }
    if response["submit"].as_bool().unwrap_or(true)
        && let Some(submit) = &c.request.submit
    {
        checkpoint(c, b, security)?;
        let m = inspect(b, &c.document, &submit.selector)?;
        validate_control(&m, false)?;
        if let Some(value) = m["submissionOrigin"].as_str()
            && origin(value)? != origin(&c.document.url)?
        {
            return Err(Error::new(-32014, "submission_origin_changed"));
        }
        let mut args = bound_control_args(c, &submit.selector)?;
        if submit.press_enter {
            args["key"] = json!("Enter");
            b.execute("locator_press", &args)?;
        } else {
            b.execute("locator_click", &args)?;
        }
    }
    Ok(())
}
pub fn validate_request(r: &Request) -> Result<()> {
    if r.fields.len() > 64 || r.options.as_ref().is_some_and(|o| o.len() > 32) {
        return Err(Error::invalid("Authentication field/option limit exceeded"));
    }
    let mut ids = BTreeSet::new();
    let mut selectors = BTreeSet::new();
    for f in &r.fields {
        if f.id.is_empty()
            || f.selector.is_empty()
            || !ids.insert(f.id.clone())
            || !selectors.insert(f.selector.clone())
        {
            return Err(Error::invalid("Duplicate/empty authentication field"));
        }
    }
    let mut options = BTreeSet::new();
    if let Some(items) = &r.options {
        for o in items {
            if o.id.is_empty()
                || !options.insert(&o.id)
                || o.fields.iter().collect::<BTreeSet<_>>().len() != o.fields.len()
                || o.fields.iter().any(|f| !ids.contains(f))
            {
                return Err(Error::invalid("Invalid authentication option"));
            }
            if let Some(selector) = &o.selector
                && (selector.is_empty() || !selectors.insert(selector.clone()))
            {
                return Err(Error::invalid("Duplicate option selector"));
            }
        }
    }
    if let Some(s) = &r.submit
        && (s.selector.is_empty() || !selectors.insert(s.selector.clone()))
    {
        return Err(Error::invalid("Duplicate submit selector"));
    }
    if r.fields.is_empty() && r.options.is_none() && r.submit.is_none() && !r.qr_code {
        return Err(Error::invalid("Authentication request has no action"));
    }
    Ok(())
}
fn prompt(r: &Request, metadata: &[Value], otp: bool, qr: Option<&qr::Symbol>, url: &str) -> Value {
    let fields = if otp {
        vec![
            json!({"id":"otp","label":"One-time code","required":r.options.is_none(),"kind":"otp"}),
        ]
    } else {
        r.fields.iter().zip(metadata).map(|(f,m)|json!({"id":f.id,"label":m["label"],"required":f.required&&r.options.is_none(),"kind":if m["type"]=="password"{"password"}else{"text"}})).collect()
    };
    json!({"fields":fields,"options":r.options.as_ref().map(|options|options.iter().map(|o|json!({"id":o.id,"label":o.label,"fields":if otp&&!o.fields.is_empty(){vec!["otp".to_string()]}else{o.fields.clone()}})).collect::<Vec<_>>()),"origin":origin(url).ok(),"qr":qr})
}
fn normalize_label(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(80)
        .collect()
}
fn token() -> Result<String> {
    let mut bytes = [0; 24];
    getrandom::fill(&mut bytes).map_err(|_| Error::action("Cannot generate challenge ID"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn wipe(v: &mut Value) {
    match v {
        Value::String(s) => s.zeroize(),
        Value::Array(a) => {
            for v in a {
                wipe(v)
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                wipe(v)
            }
        }
        _ => {}
    }
}

fn sanitize_action_error(error: Error) -> Error {
    if error.code == -32014 {
        error
    } else {
        Error::new(error.code, "Authentication action failed")
    }
}

pub fn is_otp(metadata: &[Value]) -> bool {
    metadata.len() >= 4
        && metadata.iter().all(|m| m["type"] == metadata[0]["type"])
        && (metadata
            .iter()
            .all(|m| m["autocomplete"] == "one-time-code")
            || metadata.iter().all(|m| m["inputMode"] == "numeric"))
}
