//! Response-bound download admission and scoped asynchronous wait ownership.
//! Provider callbacks are processed while a CDP command awaits its acknowledgement:
//! a paused Document may itself prevent Page.navigate from acknowledging.
use super::{Browser, Browsers, Cdp, chooser::Owner};
use crate::{Error, Result, engine::string, security::Security};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
const LIMIT: usize = 128;
const RECORD_LIMIT: usize = 1024;
pub(super) type Shared = Arc<Mutex<State>>;

// Borrowed only by the native non-download HTML callback. It cannot admit a
// command, survive the call, or turn another callback's error into retirement.
#[derive(Clone, Copy)]
pub(super) struct HtmlContinuation<'a> {
    shared: &'a Shared,
    tab: &'a str,
    session: &'a str,
    request: &'a str,
}
impl HtmlContinuation<'_> {
    pub(super) fn admits(
        &self,
        current: Option<&Shared>,
        method: &str,
        params: &Value,
        session: Option<&str>,
    ) -> bool {
        method == "Fetch.continueResponse"
            && params["requestId"].as_str() == Some(self.request)
            && session == Some(self.session)
            && current.is_some_and(|shared| Arc::ptr_eq(shared, self.shared))
            && self
                .shared
                .lock()
                .unwrap()
                .sessions
                .get(self.session)
                .is_some_and(|tab| tab.as_str() == self.tab)
    }
    pub(super) fn retired(&self, current: Option<&Shared>) -> bool {
        if !current.is_some_and(|shared| Arc::ptr_eq(shared, self.shared)) {
            return false;
        }
        let state = self.shared.lock().unwrap();
        !state.sessions.contains_key(self.session)
            && !state.sessions.values().any(|tab| tab.as_str() == self.tab)
    }
}
#[derive(Clone)]
struct Pending {
    id: String,
    session: String,
    url: String,
    continuation: Value,
    source: Option<(String, String)>,
    generation: u64,
    deferred: bool,
}
impl Pending {
    fn source_bytes(&self) -> usize {
        self.source
            .as_ref()
            .map_or(0, |(id, url)| id.len() + url.len())
    }
    fn retained_bytes(&self) -> usize {
        self.id.len()
            + self.session.len()
            + self.url.len()
            + self.continuation.to_string().len()
            + self.source_bytes()
    }
}
#[derive(Clone)]
struct Watch {
    tab: String,
    session: String,
    frame: String,
    owner: Option<Owner>,
    policy: Security,
    timeout: Duration,
    label: String,
    deadline: Option<Instant>,
    pending: Option<Pending>,
    expecting: Option<String>,
    guid: Option<String>,
    result: Option<Result<Value>>,
    terminal_at: Option<Instant>,
    cleaned: bool,
}
struct Record {
    tab: String,
    owner: Option<Owner>,
    filename: Option<String>,
    at: Instant,
}
#[derive(Default)]
pub(super) struct State {
    pub(super) owner: Option<Owner>,
    pub(super) policy: Security,
    watches: BTreeMap<String, Watch>,
    records: BTreeMap<String, Record>,
    sessions: BTreeMap<String, String>,
    legacy: BTreeMap<String, String>,
    generations: BTreeMap<String, u64>,
    configured: bool,
    pub(super) maintenance: bool,
}
fn timeout(args: &Value) -> (Duration, String) {
    let n = args
        .get("timeoutMs")
        .or_else(|| args.get("timeout_ms"))
        .and_then(Value::as_f64)
        .unwrap_or(3000.0)
        .clamp(0.0, 120000.0);
    let n = if n == 0.0 { 0.0 } else { n };
    let label = if n > 0.0 && n < 1e-6 {
        format!("{n:e}")
    } else {
        n.to_string()
    };
    (Duration::from_secs_f64(n / 1000.0), label)
}
fn identifier() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| Error::action(e.to_string()))?;
    Ok(format!(
        "download-watch-{}",
        bytes.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}
fn header<'a>(response: &'a Value, name: &str) -> Option<&'a str> {
    response["responseHeaders"].as_array()?.iter().find(|h| {
        h["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    })?["value"]
        .as_str()
}
fn mime(response: &Value) -> Option<String> {
    header(response, "content-type").map(|v| {
        v.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    })
}
fn media(mime: Option<&str>) -> bool {
    mime.is_some_and(|m| {
        m == "application/pdf" || m.starts_with("image/") || m.starts_with("video/")
    })
}
fn attachment(response: &Value) -> bool {
    let Some(value) = header(response, "content-disposition") else {
        return false;
    };
    let value = value.to_ascii_lowercase();
    value
        .strip_prefix("attachment")
        .is_some_and(|tail| tail.trim_start().is_empty() || tail.trim_start().starts_with(';'))
}
fn is_download(response: &Value, armed: bool) -> bool {
    let redirect = matches!(
        response["responseStatusCode"].as_u64(),
        Some(301 | 302 | 303 | 307 | 308)
    ) && header(response, "location").is_some();
    let mime = mime(response);
    response["resourceType"] == "Document"
        && !redirect
        && (attachment(response)
            || (armed && media(mime.as_deref()))
            || !(mime.as_deref() == Some("text/html") || media(mime.as_deref())))
}
fn overrides(response: &Value) -> Value {
    if response["responseStatusCode"].is_null()
        || !response["responseHeaders"].is_array()
        || attachment(response)
        || !media(mime(response).as_deref())
    {
        return json!({});
    }
    let mut headers = response["responseHeaders"].as_array().unwrap().clone();
    let mut found = false;
    for h in &mut headers {
        if h["name"]
            .as_str()
            .is_some_and(|n| n.eq_ignore_ascii_case("content-disposition"))
        {
            found = true;
            let value = h["value"].as_str().unwrap_or("");
            h["value"] = json!(format!(
                "attachment{}",
                value.find(';').map(|i| &value[i..]).unwrap_or("")
            ));
        }
    }
    if !found {
        headers.push(json!({"name":"Content-Disposition","value":"attachment"}));
    }
    let mut result =
        json!({"responseCode":response["responseStatusCode"],"responseHeaders":headers});
    if response["responseStatusText"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
    {
        result["responsePhrase"] = response["responseStatusText"].clone();
    }
    result
}
pub(super) fn navigation_error(tab: &str, url: &str, error: &str) -> Error {
    let clean = url::Url::parse(url)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|mut url| {
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.set_query(None);
            url.set_fragment(None);
            let root = url.path() == "/";
            let mut value = url.to_string();
            if root {
                value.pop();
            }
            value
        })
        .unwrap_or_else(|| "this page".into());
    Error::action(format!(
        "Browser Use cannot open {clean} in tab {tab}. Browser reported: {}",
        error.replace(url, &clean)
    ))
}
fn same_scope(a: &Option<Owner>, b: &Option<Owner>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => a.scope == b.scope,
        (None, None) => true,
        _ => false,
    }
}
impl State {
    fn finish(&mut self, key: &str, result: Result<Value>) {
        if let Some(w) = self.watches.get_mut(key) {
            w.result = Some(result);
            w.deadline = None;
            w.terminal_at = Some(Instant::now());
        }
    }
    fn valid(&self, key: &str, id: &str) -> bool {
        self.watches
            .get(key)
            .is_some_and(|w| w.result.is_none() && w.pending.as_ref().is_some_and(|p| p.id == id))
    }
    pub(super) fn disconnect(&mut self) {
        for w in self.watches.values_mut() {
            if w.result.is_none() {
                w.result = Some(Err(Error::new(
                    -32006,
                    "Download connection was lost; the action will not be replayed",
                )));
                w.terminal_at = Some(Instant::now());
            }
            w.pending = None;
            w.deadline = None;
        }
        self.sessions.clear();
        self.configured = false;
        self.legacy.clear();
        self.generations.clear();
    }
    pub(super) fn remove_tab(&mut self, tab: &str) {
        for w in self.watches.values_mut().filter(|w| w.tab == tab) {
            w.result = Some(Err(Error::action("Download tab detached")));
            w.pending = None;
            w.deadline = None;
            w.terminal_at = Some(Instant::now());
        }
        self.sessions.retain(|_, t| t != tab);
        self.legacy.remove(tab);
    }
    fn observe(&mut self, event: &Value) {
        let method = event["method"].as_str().unwrap_or("");
        let p = &event["params"];
        if ![
            "Browser.downloadWillBegin",
            "Browser.downloadProgress",
            "Skyre.downloadChange",
        ]
        .contains(&method)
        {
            return;
        }
        let extension = method == "Skyre.downloadChange";
        let Some(guid) = p[if extension { "id" } else { "guid" }]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 256)
        else {
            return;
        };
        let status = if method == "Browser.downloadWillBegin" {
            "started"
        } else {
            p[if extension { "status" } else { "state" }]
                .as_str()
                .unwrap_or("")
        };
        if status == "started" {
            let candidates: Vec<_> = self
                .watches
                .iter()
                .filter(|(_, w)| {
                    w.result.is_none()
                        && w.guid.is_none()
                        && w.expecting.as_deref() == p["url"].as_str()
                        && if extension {
                            event["sessionId"] == w.session && p["watchId"].as_str().is_some()
                        } else {
                            p["frameId"] == w.frame
                        }
                })
                .map(|(k, _)| k.clone())
                .collect();
            if candidates.len() != 1 {
                return;
            }
            let key = &candidates[0];
            if extension && p["watchId"] != key.as_str() {
                return;
            }
            let w = self.watches.get_mut(key).unwrap();
            w.guid = Some(guid.into());
            w.deadline = None;
            while self.records.len() >= RECORD_LIMIT {
                let oldest = self
                    .records
                    .iter()
                    .min_by_key(|(_, r)| r.at)
                    .map(|(k, _)| k.clone())
                    .unwrap();
                self.records.remove(&oldest);
            }
            self.records.insert(
                guid.into(),
                Record {
                    tab: w.tab.clone(),
                    owner: w.owner.clone(),
                    filename: p["filename"]
                        .as_str()
                        .filter(|s| s.len() <= 32768)
                        .map(str::to_owned),
                    at: Instant::now(),
                },
            );
            return;
        }
        let keys: Vec<_> = self
            .watches
            .iter()
            .filter(|(_, w)| w.result.is_none() && w.guid.as_deref() == Some(guid))
            .map(|(k, _)| k.clone())
            .collect();
        if keys.len() != 1 {
            return;
        }
        let key = &keys[0];
        let w = &self.watches[key];
        if extension && (event["sessionId"] != w.session || p["watchId"] != key.as_str()) {
            return;
        }
        if let Some(filename) = p[if extension { "filename" } else { "filePath" }]
            .as_str()
            .filter(|s| s.len() <= 32768)
            && let Some(r) = self.records.get_mut(guid)
        {
            r.filename = Some(filename.into());
        }
        match status {
            "complete" | "completed" => {
                self.finish(key, Ok(json!({"guid":guid,"download_id":guid})))
            }
            "failed" => self.finish(key, Err(Error::action(format!("Download {guid} failed.")))),
            "canceled" => self.finish(
                key,
                Err(Error::action(format!("Download {guid} was canceled."))),
            ),
            _ => {}
        }
    }
}
impl Cdp {
    fn download_fail(&mut self, request: &str, session: &str) -> Result<Value> {
        // Revocation is the only operation allowed an independent cleanup budget.
        // Never restart an uncertain response continuation or a download transfer.
        let deadlines = std::mem::take(&mut self.deadlines);
        let cleanup = *self
            .cleanup_until
            .get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
        self.deadlines.push(cleanup);
        let previous = std::mem::replace(&mut self.revoking_download, true);
        let result = self.call(
            "Fetch.failRequest",
            json!({"requestId":request,"errorReason":"BlockedByClient"}),
            Some(session),
        );
        self.deadlines = deadlines;
        self.revoking_download = previous;
        result
    }
    pub(super) fn download_event(&mut self, event: &Value) -> Result<()> {
        let disabling = event["method"] == "Fetch.requestPaused"
            && event["sessionId"]
                .as_str()
                .is_some_and(|session| self.disabling_download.contains(session));
        let result = self.process_download_event(event);
        // A queued response can arrive after the server applies our disable,
        // before its acknowledgement is consumed. Its nested continuation then
        // has no Fetch domain. Keep awaiting the enclosing disable response;
        // that acknowledgement still decides whether ownership is released.
        if disabling
            && result.as_ref().is_err_and(|error| {
                error.code == -32000 && error.message == "Fetch domain is not enabled"
            })
        {
            return Ok(());
        }
        result
    }
    fn process_download_event(&mut self, event: &Value) -> Result<()> {
        let Some(shared) = self.downloads.clone() else {
            return Ok(());
        };
        shared.lock().unwrap().observe(event);
        if event["method"] == "Page.frameNavigated"
            && event["params"]["frame"]["parentId"].is_null()
            && let Some(session) = event["sessionId"].as_str()
        {
            let mut state = shared.lock().unwrap();
            if state.sessions.contains_key(session) {
                *state.generations.entry(session.into()).or_default() += 1;
            }
        }
        if event["method"] == "Target.detachedFromTarget" {
            let mut state = shared.lock().unwrap();
            if let Some(tab) = event["params"]["sessionId"]
                .as_str()
                .and_then(|session| state.sessions.get(session))
                .cloned()
            {
                state.remove_tab(&tab);
            }
        }
        if event["method"] == "Target.targetDestroyed"
            && let Some(tab) = event["params"]["targetId"].as_str()
        {
            shared.lock().unwrap().remove_tab(tab);
        }
        if event["method"] != "Fetch.requestPaused" {
            return Ok(());
        }
        let Some(session) = event["sessionId"].as_str() else {
            return Ok(());
        };
        let p = &event["params"];
        if p["resourceType"] != "Document"
            || (p["responseStatusCode"].is_null() && p["responseErrorReason"].is_null())
        {
            return Ok(());
        }
        let Some(request) = p["requestId"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024)
        else {
            return Ok(());
        };
        let (tab, key) = {
            let state = shared.lock().unwrap();
            let Some(tab) = state.sessions.get(session) else {
                return Ok(());
            };
            (
                tab.clone(),
                state
                    .watches
                    .iter()
                    .find(|(_, w)| {
                        w.session == session
                            && w.result.is_none()
                            && w.expecting.is_none()
                            && w.deadline.is_none_or(|t| Instant::now() <= t)
                    })
                    .map(|(k, _)| k.clone()),
            )
        };
        if self.revoking_download {
            self.download_fail(request, session)?;
            return Ok(());
        }
        if !p["responseErrorReason"].is_null() {
            self.call(
                "Fetch.continueRequest",
                json!({"requestId":request}),
                Some(session),
            )?;
            return Ok(());
        }
        if !is_download(p, key.is_some()) {
            let html_continuation =
                (mime(p).as_deref() == Some("text/html")).then_some(HtmlContinuation {
                    shared: &shared,
                    tab: &tab,
                    session,
                    request,
                });
            self.call_scoped(
                "Fetch.continueResponse",
                json!({"requestId":request}),
                Some(session),
                super::CallAdmissions {
                    html_continuation,
                    ..Default::default()
                },
            )?;
            return Ok(());
        }
        let Some(key) = key else {
            self.call(
                "Fetch.failRequest",
                json!({"requestId":request,"errorReason":"BlockedByClient"}),
                Some(session),
            )?;
            return Ok(());
        };
        let url = p["request"]["url"].as_str().unwrap_or("").to_owned();
        {
            let mut state = shared.lock().unwrap();
            let continuation = overrides(p);
            let retained = state
                .watches
                .values()
                .filter_map(|w| w.pending.as_ref())
                .map(Pending::retained_bytes)
                .sum::<usize>();
            if url.len() > 65536
                || retained
                    + continuation.to_string().len()
                    + url.len()
                    + request.len()
                    + session.len()
                    > 4 * 1024 * 1024
            {
                state.finish(
                    &key,
                    Err(Error::action(
                        "Pending download response byte limit exceeded",
                    )),
                );
                drop(state);
                self.download_fail(request, session)?;
                return Ok(());
            }
            let generation = state.generations.get(session).copied().unwrap_or(0);
            let w = state.watches.get_mut(&key).unwrap();
            if let Some(previous) = w.pending.take() {
                state.finish(
                    &key,
                    Err(Error::action(
                        "The paused download response is no longer available.",
                    )),
                );
                drop(state);
                self.download_fail(request, session)?;
                if previous.id != request {
                    self.download_fail(&previous.id, &previous.session)?;
                }
                return Ok(());
            }
            if let Some(frame) = p["frameId"]
                .as_str()
                .filter(|f| !f.is_empty() && f.len() <= 256)
            {
                w.frame = frame.into();
            }
            w.pending = Some(Pending {
                id: request.into(),
                session: session.into(),
                url: url.clone(),
                continuation,
                source: None,
                generation,
                deferred: true,
            });
            // Arrival has completed; approval and later download-start stages own their clocks.
            w.deadline = None;
        };
        self.download_admit(&key)
    }
    /// Resume only from the original event or an owner-validated public poll.
    /// Periodic transport ticks never call this for already deferred responses.
    fn download_admit(&mut self, key: &str) -> Result<()> {
        let own = self.deadlines.is_empty();
        if own {
            self.cleanup_until = None;
            self.deadlines
                .push(Instant::now() + Duration::from_secs(10));
        }
        let result = self.download_admit_inner(key);
        if own {
            self.deadlines.pop();
        }
        result
    }
    fn download_admit_inner(&mut self, key: &str) -> Result<()> {
        let Some(shared) = self.downloads.clone() else {
            return Ok(());
        };
        let Some(watch) = shared
            .lock()
            .unwrap()
            .watches
            .get(key)
            .filter(|w| w.result.is_none())
            .cloned()
        else {
            return Ok(());
        };
        let Some(pending) = watch.pending.clone() else {
            return Ok(());
        };
        let request = pending.id.as_str();
        let session = pending.session.as_str();
        let tab = watch.tab.as_str();
        let url = pending.url.clone();
        // The callback may block for its bounded host review. No state lock spans
        // it. Drain incoming replacement/detach events before the sensitive send.
        let approval = (|| -> Result<bool> {
            let before = self.call("Target.getTargetInfo", json!({"targetId":tab}), None)?;
            let source = &before["targetInfo"];
            let target_id = string(source, "targetId")?;
            let source_url = string(source, "url")?;
            if target_id.is_empty() || target_id.len() > 256 || source_url.len() > 65536 {
                return Err(Error::action(
                    "Download source identity exceeds the supported bound",
                ));
            }
            let identity = (target_id.to_owned(), source_url.to_owned());
            let generation = shared
                .lock()
                .unwrap()
                .generations
                .get(session)
                .copied()
                .unwrap_or(0);
            if generation != pending.generation
                || pending.source.as_ref().is_some_and(|old| old != &identity)
            {
                return Err(Error::action(
                    "Download source document changed before approval",
                ));
            }
            if !shared.lock().unwrap().valid(key, request) {
                return Ok(false);
            }
            {
                let mut state = shared.lock().unwrap();
                let retained = state
                    .watches
                    .values()
                    .filter_map(|w| w.pending.as_ref())
                    .map(Pending::retained_bytes)
                    .sum::<usize>();
                if retained.saturating_sub(pending.source_bytes())
                    + identity.0.len()
                    + identity.1.len()
                    > 4 * 1024 * 1024
                {
                    return Err(Error::action(
                        "Pending download response byte limit exceeded",
                    ));
                }
                if let Some(w) = state.watches.get_mut(key)
                    && let Some(p) = w.pending.as_mut()
                {
                    p.source = Some(identity);
                }
            }
            if !watch.policy.download_approval_ready()? {
                return Ok(false);
            }
            if let Some(w) = shared.lock().unwrap().watches.get_mut(key)
                && let Some(p) = w.pending.as_mut()
            {
                p.deferred = false;
            }
            let deadline = self
                .deadlines
                .last()
                .copied()
                .unwrap_or_else(|| Instant::now() + Duration::from_secs(10));
            let suspended_before = watch.policy.download_suspended_duration()?;
            let approved = watch.policy.check_download_until(
                source["url"].as_str().unwrap_or(""),
                &url,
                deadline,
            );
            let suspended = watch
                .policy
                .download_suspended_duration()?
                .checked_sub(suspended_before)
                .ok_or_else(|| Error::action("Download suspension clock moved backwards"))?;
            // Exclude only actual, trusted human-approval intervals. This also
            // restores network cleanup time after denial or cancellation.
            let deadlines = self
                .deadlines
                .iter()
                .map(|deadline| {
                    deadline
                        .checked_add(suspended)
                        .ok_or_else(|| Error::action("CDP approval deadline overflow"))
                })
                .collect::<Result<Vec<_>>>()?;
            self.deadlines = deadlines;
            let deadline = deadline
                .checked_add(suspended)
                .ok_or_else(|| Error::action("Download admission deadline overflow"))?;
            approved?;
            if Instant::now() >= deadline {
                return Err(Error::new(-32006, "Download admission deadline expired"));
            }
            self.poll_events()?;
            let after = self.call("Target.getTargetInfo", json!({"targetId":tab}), None)?;
            let current = &after["targetInfo"];
            if source["url"] != current["url"]
                || source["targetId"] != current["targetId"]
                || generation
                    != shared
                        .lock()
                        .unwrap()
                        .generations
                        .get(session)
                        .copied()
                        .unwrap_or(0)
            {
                return Err(Error::action(
                    "Download source document changed during approval",
                ));
            }
            Ok(true)
        })();
        if !shared.lock().unwrap().valid(key, request) {
            return Ok(());
        }
        if matches!(approval, Ok(false)) {
            return Ok(());
        }
        if let Err(error) = approval {
            let cleanup = self.download_fail(request, session);
            let mut state = shared.lock().unwrap();
            if let Some(w) = state.watches.get_mut(key) {
                w.pending = None;
            }
            state.finish(key, Err(cleanup.err().unwrap_or(error)));
            return Ok(());
        }
        if self.download_extension
            && let Err(error) = self.call(
                "Skyre.downloadExpect",
                json!({"url":url,"watchId":key}),
                Some(session),
            )
        {
            let _ = self.download_fail(request, session);
            shared.lock().unwrap().finish(key, Err(error));
            return Ok(());
        }
        if !shared.lock().unwrap().valid(key, request) {
            return Ok(());
        }
        {
            let mut state = shared.lock().unwrap();
            let w = state.watches.get_mut(key).unwrap();
            w.expecting = Some(url);
            w.deadline = Some(Instant::now() + w.timeout);
        }
        let mut params = pending.continuation;
        params["requestId"] = json!(request);
        let continued = self.call("Fetch.continueResponse", params, Some(session));
        let mut state = shared.lock().unwrap();
        if let Some(w) = state.watches.get_mut(key) {
            let same = w.pending.as_ref().is_some_and(|p| p.id == request);
            w.pending = None;
            if let Err(error) = continued {
                state.finish(key, Err(error));
            } else if !same {
                state.finish(
                    key,
                    Err(Error::action(
                        "The paused download response is no longer available.",
                    )),
                );
            }
        }
        let _ = tab;
        Ok(())
    }
}
impl Browsers {
    pub fn set_download_security(&mut self, security: Security) {
        self.download_security = security.clone();
        for browser in self.providers.values_mut() {
            browser.surface.downloads.lock().unwrap().policy = security.clone();
        }
    }
}
impl Browser {
    pub(super) fn surface_download_interception_owned(&self, session: &str) -> bool {
        self.surface
            .downloads
            .lock()
            .unwrap()
            .sessions
            .contains_key(session)
    }
    pub(super) fn navigate_url(&mut self, tab: &str, url: &str) -> Result<Value> {
        self.navigate_url_scoped(tab, url, None)
    }
    pub(super) fn navigate_url_scoped(
        &mut self,
        tab: &str,
        url: &str,
        automatic: Option<&super::beforeunload::Operation<'_>>,
    ) -> Result<Value> {
        let result = match automatic
            .filter(|operation| operation.kind() == super::beforeunload::Kind::Navigate)
        {
            Some(operation) => {
                let session = self.command_session(tab, "Page.navigate")?;
                self.call_beforeunload(
                    "Page.navigate",
                    json!({"url":url}),
                    Some(&session),
                    operation,
                )?
            }
            None => self.tab(tab, "Page.navigate", json!({"url":url}))?,
        };
        if let Some(error) = result["errorText"].as_str().filter(|text| !text.is_empty()) {
            return Err(navigation_error(tab, url, error));
        }
        Ok(result)
    }
    pub(super) fn attach_download_interception(&mut self, tab: &str, session: &str) -> Result<()> {
        {
            let mut state = self.surface.downloads.lock().unwrap();
            if state.sessions.contains_key(session) {
                return Ok(());
            }
            if state.sessions.len() >= 128 {
                return Err(Error::action(
                    "Controlled document interception limit exceeded",
                ));
            }
            state.sessions.insert(session.into(), tab.into());
        }
        if let Err(error) = self.call(
            "Fetch.enable",
            json!({"patterns":[{"resourceType":"Document","requestStage":"Response"}]}),
            Some(session),
        ) {
            self.surface
                .downloads
                .lock()
                .unwrap()
                .sessions
                .remove(session);
            return Err(error);
        }
        Ok(())
    }
    pub(super) fn shutdown_downloads(&mut self, tab: Option<&str>) -> Result<()> {
        self.cancel_downloads(tab, None, None);
        let mut error = self.download_maintenance().err();
        let sessions: Vec<_> = self
            .surface
            .downloads
            .lock()
            .unwrap()
            .sessions
            .iter()
            .filter(|(_, t)| tab.is_none_or(|tab| *t == tab))
            .map(|(s, _)| s.clone())
            .collect();
        for session in sessions {
            match self.call_maintenance("Fetch.disable", json!({}), Some(&session)) {
                Err(failure) => {
                    error.get_or_insert(failure);
                }
                Ok(_) => {
                    self.surface
                        .downloads
                        .lock()
                        .unwrap()
                        .sessions
                        .remove(&session);
                }
            }
        }
        if tab.is_none() {
            self.surface.downloads.lock().unwrap().records.clear();
        }
        error.map_or(Ok(()), Err)
    }

    pub(super) fn download_command(
        &mut self,
        method: &str,
        tab: &str,
        args: &Value,
    ) -> Result<Value> {
        match method {
            "download_arm" => self.download_arm(tab, args),
            "downloads_enable" => {
                let watch = self.download_arm(tab, args)?;
                self.surface
                    .downloads
                    .lock()
                    .unwrap()
                    .legacy
                    .insert(tab.into(), watch["watchId"].as_str().unwrap().into());
                Ok(watch)
            }
            "downloads_disable" => {
                self.cancel_downloads(Some(tab), None, None);
                self.download_maintenance()?;
                Ok(Value::Null)
            }
            "download_cancel" => {
                let key = string(args, "watchId")?;
                self.download_watch(tab, key)?;
                self.surface
                    .downloads
                    .lock()
                    .unwrap()
                    .finish(key, Err(Error::new(-32800, "Download wait cancelled")));
                self.download_maintenance()?;
                Ok(Value::Null)
            }
            "download_poll" => self.download_poll(tab, string(args, "watchId")?),
            "wait_for_download" => {
                let watch = if let Some(key) = args["watchId"].as_str() {
                    key.to_owned()
                } else {
                    self.surface
                        .downloads
                        .lock()
                        .unwrap()
                        .legacy
                        .remove(tab)
                        .unwrap_or_default()
                };
                let key = if watch.is_empty() {
                    self.download_arm(tab, args)?["watchId"]
                        .as_str()
                        .unwrap()
                        .into()
                } else {
                    watch
                };
                loop {
                    let value = self.download_poll(tab, &key)?;
                    if value["pending"] != true {
                        return Ok(value["value"].clone());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
            "download_path" => {
                let id = string(args, "downloadId").or_else(|_| string(args, "download_id"))?;
                if id.is_empty() {
                    return Err(Error::invalid("download_id must be a non-empty string"));
                }
                let state = self.surface.downloads.lock().unwrap();
                let record = state
                    .records
                    .get(id)
                    .filter(|r| r.tab == tab && same_scope(&r.owner, &state.owner));
                Ok(json!({"path":record.and_then(|r|r.filename.as_deref())}))
            }
            _ => Err(Error::unsupported("Unknown download operation")),
        }
    }
    fn download_arm(&mut self, tab: &str, args: &Value) -> Result<Value> {
        self.ensure_context()?;
        let key = identifier()?;
        let (duration, label) = timeout(args);
        {
            let state = self.surface.downloads.lock().unwrap();
            if state.watches.len() >= LIMIT {
                return Err(Error::action("Download watch limit exceeded"));
            }
            if state
                .watches
                .values()
                .any(|w| w.tab == tab && w.result.is_none())
            {
                return Err(Error::action("A download wait already owns this tab"));
            }
        }
        let session = self.session(tab)?;
        self.page(tab)?;
        let frame = self.frame(tab, &json!({}))?;
        let frame_id = string(&frame, "id")?.to_owned();
        let _ = string(&frame, "url")?;
        let configured = self.surface.downloads.lock().unwrap().configured;
        if !configured && !self.extension {
            // Generic endpoints keep their browser's normal download destination.
            // Only independently owned IAB contexts select our private directory.
            let params = if self.iab.is_some() {
                json!({"behavior":"allowAndName","eventsEnabled":true,"downloadPath":self.surface.artifacts.directory()?})
            } else {
                json!({"behavior":"default","eventsEnabled":true})
            };
            self.call_for_tab(tab, "Browser.setDownloadBehavior", params, None)?;
            self.surface.downloads.lock().unwrap().configured = true;
        }
        {
            let mut state = self.surface.downloads.lock().unwrap();
            let owner = state.owner.clone();
            let policy = state.policy.clone();
            state.sessions.insert(session.clone(), tab.into());
            state.watches.insert(
                key.clone(),
                Watch {
                    tab: tab.into(),
                    session: session.clone(),
                    frame: frame_id,
                    owner,
                    policy,
                    timeout: duration,
                    label,
                    deadline: Some(Instant::now() + duration),
                    pending: None,
                    expecting: None,
                    guid: None,
                    result: None,
                    terminal_at: None,
                    cleaned: false,
                },
            );
        }
        // Like the original request waiter, its deadline begins after setup.
        if let Some(w) = self.surface.downloads.lock().unwrap().watches.get_mut(&key)
            && w.expecting.is_none()
            && w.result.is_none()
        {
            w.deadline = Some(Instant::now() + duration);
        }
        Ok(json!({"watchId":key}))
    }
    fn download_watch(&self, tab: &str, key: &str) -> Result<Watch> {
        let state = self.surface.downloads.lock().unwrap();
        state
            .watches
            .get(key)
            .filter(|w| w.tab == tab && w.owner == state.owner)
            .cloned()
            .ok_or_else(|| Error::action("Download wait does not belong to this cell"))
    }
    fn download_poll(&mut self, tab: &str, key: &str) -> Result<Value> {
        self.download_watch(tab, key)?;
        self.poll_events()?;
        self.download_maintenance()?;
        let watch = self.download_watch(tab, key)?;
        if watch.result.is_none() && watch.pending.as_ref().is_some_and(|p| p.deferred) {
            self.client()?.download_admit(key)?;
            self.poll_events()?;
            self.download_maintenance()?;
        }
        let watch = self.download_watch(tab, key)?;
        if let Some(result) = watch.result {
            self.surface.downloads.lock().unwrap().watches.remove(key);
            return result.map(|value| json!({"pending":false,"value":value}));
        }
        Ok(json!({"pending":true}))
    }
    pub(super) fn tick_downloads(&mut self) {
        let active = !self.surface.downloads.lock().unwrap().sessions.is_empty();
        if active {
            let _ = self.poll_events();
            let _ = self.download_maintenance();
        }
    }
    pub(super) fn cancel_downloads(
        &mut self,
        tab: Option<&str>,
        scope: Option<&str>,
        cell: Option<u64>,
    ) {
        let mut state = self.surface.downloads.lock().unwrap();
        let keys: Vec<_> = state
            .watches
            .iter()
            .filter(|(_, w)| {
                tab.is_none_or(|t| w.tab == t)
                    && scope.is_none_or(|s| {
                        w.owner
                            .as_ref()
                            .is_some_and(|o| o.scope == s && cell.is_none_or(|c| o.cell == c))
                    })
            })
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            state.finish(&key, Err(Error::new(-32800, "Download wait cancelled")));
        }
    }
    pub(super) fn download_maintenance(&mut self) -> Result<()> {
        let shared = self.surface.downloads.clone();
        {
            let mut state = shared.lock().unwrap();
            if state.maintenance {
                return Ok(());
            }
            state.maintenance = true;
        }
        let result = self.download_cleanup();
        shared.lock().unwrap().maintenance = false;
        result
    }
    fn download_cleanup(&mut self) -> Result<()> {
        let shared = self.surface.downloads.clone();
        let (pending, sessions, disable) = {
            let mut state = shared.lock().unwrap();
            let now = Instant::now();
            for w in state.watches.values_mut() {
                if w.result.is_none() && w.deadline.is_some_and(|t| now >= t) {
                    w.result = Some(Err(Error::action(format!(
                        "Timed out after {}ms waiting for download.",
                        w.label
                    ))));
                    w.deadline = None;
                    w.terminal_at = Some(now);
                }
            }
            let pending = state
                .watches
                .values_mut()
                .filter(|w| w.result.is_some())
                .filter_map(|w| w.pending.take())
                .collect::<Vec<_>>();
            let active: BTreeSet<_> = state
                .watches
                .values()
                .filter(|w| w.result.is_none())
                .map(|w| w.session.clone())
                .collect();
            let sessions = state
                .watches
                .values_mut()
                .filter(|w| w.result.is_some() && !w.cleaned)
                .map(|w| {
                    w.cleaned = true;
                    w.session.clone()
                })
                .collect::<BTreeSet<_>>();
            let disable = active.is_empty() && state.configured;
            state.watches.retain(|_, w| {
                w.terminal_at
                    .is_none_or(|t| now.duration_since(t) < Duration::from_secs(30))
            });
            (pending, sessions, disable)
        };
        let mut failure = None;
        for p in pending {
            let _ = &p.url;
            if let Err(e) = self.call_maintenance(
                "Fetch.failRequest",
                json!({"requestId":p.id,"errorReason":"BlockedByClient"}),
                Some(&p.session),
            ) {
                failure.get_or_insert(e);
            }
        }
        for session in sessions {
            if self.extension {
                let _ = self.call_maintenance("Skyre.downloadForget", json!({}), Some(&session));
            }
        }
        if disable {
            let result = self.call(
                "Browser.setDownloadBehavior",
                json!({"behavior":"default","eventsEnabled":false}),
                None,
            );
            shared.lock().unwrap().configured = false;
            if let Err(e) = result {
                failure.get_or_insert(e);
            }
        }
        if let Some(error) = failure {
            let mut state = shared.lock().unwrap();
            for w in state.watches.values_mut().filter(|w| w.result.is_some()) {
                w.result = Some(Err(error.clone()));
            }
            return Err(error);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_poll_restores_short_read_after_nested_response_without_extending_budget() {
        use std::{net::TcpListener, sync::mpsc, thread};
        use tungstenite::Message;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (ready, queued) = mpsc::channel();
        let (stop, stopped) = mpsc::channel();
        let peer = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .send(Message::text(
                    json!({
                        "method":"Fetch.requestPaused", "sessionId":"root",
                        "params":{"requestId":"ordinary-document", "resourceType":"Document",
                            "responseStatusCode":200,
                            "responseHeaders":[{"name":"Content-Type","value":"text/html"}]}
                    })
                    .to_string(),
                ))
                .unwrap();
            ready.send(()).unwrap();
            let request: Value =
                serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
            assert_eq!(request["method"], "Fetch.continueResponse");
            // Deliver the event before acknowledging the nested call. This
            // guarantees the client has read it before the idle-poll assertion;
            // a successful TCP write alone does not guarantee that a later
            // frame reaches the peer inside a single 1ms polling window.
            socket
                .send(Message::text(
                    json!({"method":"Page.loadEventFired","sessionId":"root","params":{}})
                        .to_string(),
                ))
                .unwrap();
            socket
                .send(Message::text(
                    json!({"id":request["id"],"result":{}}).to_string(),
                ))
                .unwrap();
            // Keep an idle but connected stream after the nested reply. A stale
            // socket timeout must not make one nonblocking poll consume its
            // entire enclosing command budget.
            let _ = stopped.recv_timeout(Duration::from_secs(5));
        });
        let mut client = Cdp::connect(&endpoint).unwrap();
        let mut state = State::default();
        state.sessions.insert("root".into(), "tab".into());
        client.downloads = Some(Arc::new(Mutex::new(state)));
        queued.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(1500);
        client.deadlines.push(deadline);
        let start = Instant::now();
        let result = client.poll_events();
        let elapsed = start.elapsed();
        let _ = stop.send(());
        peer.join().unwrap();
        result.unwrap();
        assert_eq!(client.deadlines, vec![deadline]);
        assert!(
            client
                .events
                .iter()
                .any(|event| event["method"] == "Page.loadEventFired")
        );
        assert!(
            elapsed < Duration::from_millis(250),
            "Idle poll blocked for {elapsed:?}"
        );
    }
    #[test]
    fn extension_terminal_callback_requires_causal_watch_and_bounds_initial_filename() {
        let mut state = State::default();
        state.watches.insert(
            "watch".into(),
            Watch {
                tab: "tab".into(),
                session: "root".into(),
                frame: "frame".into(),
                owner: None,
                policy: Security::default(),
                timeout: Duration::from_secs(1),
                label: "1000".into(),
                deadline: Some(Instant::now() + Duration::from_secs(1)),
                pending: None,
                expecting: Some("https://owned.test/file".into()),
                guid: None,
                result: None,
                terminal_at: None,
                cleaned: false,
            },
        );
        state.observe(&json!({"method":"Skyre.downloadChange","sessionId":"root","params":{"id":"1","watchId":"watch","url":"https://owned.test/file","status":"started","filename":"x".repeat(32769)}}));
        assert_eq!(state.watches["watch"].guid.as_deref(), Some("1"));
        assert!(state.records["1"].filename.is_none());
        state.observe(&json!({"method":"Skyre.downloadChange","sessionId":"root","params":{"id":"1","watchId":"stale-watch","status":"complete","filename":"/wrong"}}));
        assert!(state.watches["watch"].result.is_none());
        assert!(state.records["1"].filename.is_none());
        state.observe(&json!({"method":"Skyre.downloadChange","sessionId":"root","params":{"id":"1","watchId":"watch","status":"complete","filename":"/owned"}}));
        assert!(state.watches["watch"].result.as_ref().unwrap().is_ok());
        assert_eq!(state.records["1"].filename.as_deref(), Some("/owned"));
    }
    #[test]
    fn download_matches_captured_classifier_overrides_and_timeout_rows() {
        let oracle: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_downloads.json")).unwrap();
        for row in oracle["classifications"].as_array().unwrap() {
            assert_eq!(
                is_download(&row["response"], row["armed"].as_bool().unwrap()),
                row["download"],
                "{row}"
            );
            assert_eq!(overrides(&row["response"]), row["overrides"], "{row}");
        }
        for row in oracle["timeouts"].as_array().unwrap() {
            assert_eq!(timeout(&json!({"timeoutMs":row["value"]})).1, row["label"]);
        }
    }
    #[test]
    fn download_classifier_and_override_match_captured_mime_contract() {
        let response = |mime: &str| json!({"resourceType":"Document","responseStatusCode":200,"responseHeaders":[{"name":"content-type","value":mime}]});
        assert!(!is_download(&response("text/html"), true));
        assert!(!is_download(&response("image/png"), false));
        assert!(is_download(&response("image/png"), true));
        assert!(is_download(&response("application/octet-stream"), false));
        let result = overrides(&response("application/pdf"));
        assert_eq!(result["responseHeaders"][1]["value"], "attachment");
        let mut redirect = response("application/octet-stream");
        redirect["responseStatusCode"] = json!(302);
        redirect["responseHeaders"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":"location","value":"/final"}));
        assert!(!is_download(&redirect, true));
    }
    #[test]
    fn download_timeout_matches_captured_clamped_fractional_labels() {
        for (v, label) in [
            (json!(-1), "0"),
            (json!(0.5), "0.5"),
            (json!(1e-7), "1e-7"),
            (json!(200000), "120000"),
            (json!("5"), "3000"),
            (Value::Null, "3000"),
        ] {
            assert_eq!(timeout(&json!({"timeoutMs":v})).1, label);
        }
    }
}

#[cfg(test)]
mod html_continuation_tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc, thread};
    use tungstenite::{Message, WebSocket};

    #[derive(Clone, Copy, Debug)]
    enum Schedule {
        Retired,
        Destroyed,
        Deferred,
        OuterError,
        Active,
        OtherRetired,
        OtherCode,
        NestedActiveError,
        Media,
        Public,
    }

    struct Observation {
        result: Result<Value>,
        calls: Vec<Value>,
        sessions: BTreeMap<String, String>,
        events: Vec<Value>,
        request_state_empty: bool,
    }

    fn send(socket: &mut WebSocket<std::net::TcpStream>, value: Value) {
        socket.send(Message::text(value.to_string())).unwrap();
    }

    fn read(socket: &mut WebSocket<std::net::TcpStream>, calls: &mut Vec<Value>) -> Value {
        let value: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        calls.push(value.clone());
        value
    }

    fn paused(session: &str, request: &str, mime: &str) -> Value {
        json!({"method":"Fetch.requestPaused","sessionId":session,
            "params":{"requestId":request,"resourceType":"Document",
                "responseStatusCode":200,"request":{"url":"http://owned.test/"},
                "responseHeaders":[{"name":"Content-Type","value":mime}]}})
    }

    fn detached(session: &str, tab: &str) -> Value {
        json!({"method":"Target.detachedFromTarget",
            "params":{"sessionId":session,"targetId":tab}})
    }

    fn failure(id: &Value, code: i32, message: &str) -> Value {
        json!({"id":id,"error":{"code":code,"message":message}})
    }

    fn observe(schedule: Schedule) -> Observation {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (stop, stopped) = mpsc::channel();
        let peer = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "Owned setup deadline expired");
                        thread::yield_now();
                    }
                    Err(error) => panic!("Owned accept failed: {error}"),
                }
            };
            // Darwin can inherit O_NONBLOCK from the listener. Keep the existing
            // five-second connection bound while completing the handshake.
            stream.set_nonblocking(false).unwrap();
            let remaining_ms = deadline
                .saturating_duration_since(Instant::now())
                .as_millis();
            assert!(remaining_ms > 0, "Owned setup deadline expired");
            let remaining = Duration::from_millis(remaining_ms as u64);
            stream.set_read_timeout(Some(remaining)).unwrap();
            stream.set_write_timeout(Some(remaining)).unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .get_mut()
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
                .get_mut()
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut calls = Vec::new();
            let outer = read(&mut socket, &mut calls);
            if matches!(schedule, Schedule::Public) {
                assert_eq!(outer["method"], "Fetch.continueResponse");
                assert_eq!(outer["sessionId"], "root");
                assert_eq!(outer["params"]["requestId"], "ordinary-html");
                send(&mut socket, detached("root", "tab"));
                send(
                    &mut socket,
                    failure(&outer["id"], -32001, "Session with given id not found."),
                );
            } else {
                assert_eq!(outer["method"], "Target.closeTarget");
                assert_eq!(outer["params"], json!({"targetId":"tab"}));
                send(
                    &mut socket,
                    paused(
                        "root",
                        "ordinary-html",
                        if matches!(schedule, Schedule::Media) {
                            "image/png"
                        } else {
                            "text/html"
                        },
                    ),
                );
                // This real outer response is sent before the continuation
                // response. The nested client must defer and later consume it.
                let outer_reply = if matches!(schedule, Schedule::OuterError) {
                    failure(&outer["id"], -32123, "Owned close refusal")
                } else {
                    json!({"id":outer["id"],"result":{
                        "success":true,"owned":"actual-close-ack"}})
                };
                send(&mut socket, outer_reply);
                let continuation = read(&mut socket, &mut calls);
                assert_eq!(continuation["method"], "Fetch.continueResponse");
                assert_eq!(continuation["sessionId"], "root");
                assert_eq!(continuation["params"], json!({"requestId":"ordinary-html"}));
                match schedule {
                    Schedule::Active => {}
                    Schedule::OtherRetired => {
                        send(&mut socket, detached("other-root", "other-tab"));
                    }
                    Schedule::Destroyed => send(
                        &mut socket,
                        json!({"method":"Target.targetDestroyed","params":{"targetId":"tab"}}),
                    ),
                    _ => send(&mut socket, detached("root", "tab")),
                }
                if matches!(schedule, Schedule::Deferred | Schedule::NestedActiveError) {
                    send(&mut socket, paused("other-root", "other-html", "text/html"));
                    let nested = read(&mut socket, &mut calls);
                    assert_eq!(nested["method"], "Fetch.continueResponse");
                    assert_eq!(nested["sessionId"], "other-root");
                    assert_eq!(nested["params"], json!({"requestId":"other-html"}));
                    if matches!(schedule, Schedule::Deferred) {
                        // Also exercise a matching continuation reply that is
                        // deferred while another callback succeeds.
                        send(
                            &mut socket,
                            failure(
                                &continuation["id"],
                                -32001,
                                "Session with given id not found.",
                            ),
                        );
                        send(&mut socket, json!({"id":nested["id"],"result":{}}));
                    } else {
                        // The first owner is already gone, but this nested
                        // callback belongs to a different, still-active tab.
                        send(
                            &mut socket,
                            failure(&nested["id"], -32001, "Active nested continuation refusal"),
                        );
                    }
                } else {
                    send(
                        &mut socket,
                        failure(
                            &continuation["id"],
                            if matches!(schedule, Schedule::OtherCode) {
                                -32000
                            } else {
                                -32001
                            },
                            "Session with given id not found.",
                        ),
                    );
                }
            }
            // Keep the connection alive until the caller has obtained its
            // result; a successful write alone does not establish receipt.
            stopped.recv_timeout(Duration::from_secs(5)).unwrap();
            calls
        });
        let mut client = Cdp::connect(&endpoint).unwrap();
        let mut state = State::default();
        state.sessions.insert("root".into(), "tab".into());
        state
            .sessions
            .insert("other-root".into(), "other-tab".into());
        let shared = Arc::new(Mutex::new(state));
        client.downloads = Some(shared.clone());
        let result = if matches!(schedule, Schedule::Public) {
            client.call(
                "Fetch.continueResponse",
                json!({"requestId":"ordinary-html"}),
                Some("root"),
            )
        } else {
            client.call("Target.closeTarget", json!({"targetId":"tab"}), None)
        };
        let request_state_empty =
            client.waiting.is_empty() && client.responses.is_empty() && client.deadlines.is_empty();
        let events = client
            .drain_events()
            .into_iter()
            .map(|(event, _)| event)
            .collect();
        let sessions = shared.lock().unwrap().sessions.clone();
        let _ = stop.send(());
        drop(client);
        // Join before asserting the client result: worker failures remain
        // failures without a second panic from a fixture destructor.
        let calls = peer.join().unwrap();
        Observation {
            result,
            calls,
            sessions,
            events,
            request_state_empty,
        }
    }

    #[test]
    fn html_retirement_preserves_actual_outer_ack_and_deferred_continuation() {
        for schedule in [Schedule::Retired, Schedule::Destroyed, Schedule::Deferred] {
            let observation = observe(schedule);
            assert_eq!(
                observation.result.unwrap(),
                json!({"success":true,"owned":"actual-close-ack"}),
                "{schedule:?}"
            );
            assert!(observation.request_state_empty, "{schedule:?}");
            assert_eq!(
                observation.sessions,
                BTreeMap::from([("other-root".into(), "other-tab".into())])
            );
            assert!(observation.events.iter().any(|event| {
                event["method"] == "Target.detachedFromTarget"
                    || event["method"] == "Target.targetDestroyed"
            }));
            assert_eq!(
                observation
                    .calls
                    .iter()
                    .map(|call| call["method"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                if matches!(schedule, Schedule::Deferred) {
                    vec![
                        "Target.closeTarget",
                        "Fetch.continueResponse",
                        "Fetch.continueResponse",
                    ]
                } else {
                    vec!["Target.closeTarget", "Fetch.continueResponse"]
                },
                "{schedule:?}"
            );
        }
    }

    #[test]
    fn html_retirement_preserves_outer_active_nested_and_public_errors() {
        for schedule in [
            Schedule::OuterError,
            Schedule::Active,
            Schedule::OtherRetired,
            Schedule::OtherCode,
            Schedule::NestedActiveError,
            Schedule::Media,
            Schedule::Public,
        ] {
            let observation = observe(schedule);
            let error = observation.result.unwrap_err();
            let expected = match schedule {
                Schedule::OuterError => (-32123, "Owned close refusal"),
                Schedule::OtherCode => (-32000, "Session with given id not found."),
                Schedule::NestedActiveError => (-32001, "Active nested continuation refusal"),
                _ => (-32001, "Session with given id not found."),
            };
            assert_eq!(
                (error.code, error.message.as_str()),
                expected,
                "{schedule:?}"
            );
            assert!(observation.request_state_empty, "{schedule:?}");
            assert_eq!(
                observation.sessions.contains_key("root"),
                matches!(schedule, Schedule::Active | Schedule::OtherRetired),
                "{schedule:?}"
            );
            assert_eq!(
                observation.sessions.contains_key("other-root"),
                !matches!(schedule, Schedule::OtherRetired),
                "{schedule:?}"
            );
            assert_eq!(
                observation.calls.len(),
                match schedule {
                    Schedule::Public => 1,
                    Schedule::NestedActiveError => 3,
                    _ => 2,
                }
            );
            assert!(observation.calls.iter().all(|call| {
                matches!(
                    call["method"].as_str(),
                    Some("Target.closeTarget" | "Fetch.continueResponse")
                )
            }));
        }
    }
}
