//! Browser API compatibility behavior backed by the owned CDP session.
use super::Browser;
use crate::{Error, Result, engine::string};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct State {
    next_watch: u64,
    watches: BTreeMap<String, Watch>,
    pub viewport: Option<Value>,
    network_enabled: BTreeSet<String>,
    pending: BTreeMap<String, BTreeSet<String>>,
    network_activity: BTreeMap<String, Instant>,
}
#[derive(Clone)]
struct Watch {
    tab: String,
    session: String,
    frame: Option<String>,
    owner: Option<super::chooser::Owner>,
    url: Option<String>,
    state: Option<String>,
    waiting_url: bool,
    timeout: Duration,
    deadline: Instant,
    result: Option<Result<Value>>,
    terminal_at: Option<Instant>,
}
impl Watch {
    fn event(&mut self, event: &Value) {
        if event["sessionId"].as_str() != Some(&self.session) {
            return;
        }
        self.expire();
        if self.result.is_some() {
            return;
        }
        let method = event["method"].as_str().unwrap_or("");
        if self.waiting_url {
            let params = &event["params"];
            let url = match method {
                // Retained a1 matches same-document URL events without an additional frame predicate.
                "Page.navigatedWithinDocument" => params["url"].as_str(),
                "Page.frameNavigated"
                    if self
                        .frame
                        .as_ref()
                        .map_or(params["frame"]["parentId"].is_null(), |frame| {
                            params["frame"]["id"] == *frame
                        }) =>
                {
                    params["frame"]["url"].as_str()
                }
                _ => None,
            };
            if let Some(url) = url {
                self.advance(&json!({"url":url,"state":"loading"}));
            }
        } else if method == "Page.loadEventFired"
            || self.state.as_deref() == Some("domcontentloaded")
                && method == "Page.domContentEventFired"
        {
            self.advance(&json!({"state":"complete"}));
        }
    }
    fn advance(&mut self, value: &Value) {
        if self.result.is_some() {
            return;
        }
        if self.waiting_url {
            if !self.url.as_ref().is_some_and(|url| {
                super::surface::url_matches(url, value["url"].as_str().unwrap_or(""))
            }) {
                self.expire();
                return;
            }
            self.waiting_url = false;
            // The original URL wait starts a fresh load-state budget after URL match.
            self.deadline = Instant::now() + self.timeout;
        }
        let ready = self.state.as_deref().is_none_or(|state| {
            value["state"] == "complete"
                || state == "domcontentloaded" && value["state"] == "interactive"
        });
        if ready {
            self.result = Some(Ok(Value::Null));
            self.terminal_at = Some(Instant::now());
        } else {
            self.expire();
        }
    }
    fn expire(&mut self) {
        if self.result.is_none() && Instant::now() >= self.deadline {
            let message = if self.waiting_url {
                format!(
                    "Timed out waiting for URL {} in tab {}.",
                    self.url.as_deref().unwrap_or(""),
                    self.tab
                )
            } else {
                format!(
                    "Timed out waiting for {} in tab {}.",
                    self.state.as_deref().unwrap_or("load"),
                    self.tab
                )
            };
            self.result = Some(Err(Error::action(message)));
            self.terminal_at = Some(Instant::now());
        }
    }
}
impl State {
    pub(super) fn cancel_owner(&mut self, scope: &str, cell: Option<u64>) {
        self.watches.retain(|_, watch| {
            !watch.owner.as_ref().is_some_and(|owner| {
                owner.scope == scope && cell.is_none_or(|cell| owner.cell == cell)
            })
        });
    }
    pub(super) fn tick(&mut self) {
        for watch in self.watches.values_mut() {
            watch.expire();
        }
        self.watches.retain(|_, watch| {
            watch
                .terminal_at
                .is_none_or(|at| at.elapsed() < Duration::from_secs(60))
        });
    }
    pub fn event(&mut self, event: &Value) {
        for watch in self.watches.values_mut() {
            watch.event(event);
        }
        let Some(session) = event["sessionId"].as_str() else {
            return;
        };
        let Some(request) = event["params"]["requestId"].as_str() else {
            return;
        };
        match event["method"].as_str() {
            Some("Network.requestWillBeSent") => {
                self.pending
                    .entry(session.into())
                    .or_default()
                    .insert(request.into());
            }
            Some("Network.loadingFinished" | "Network.loadingFailed") => {
                self.pending
                    .entry(session.into())
                    .or_default()
                    .remove(request);
            }
            _ => return,
        }
        self.network_activity.insert(session.into(), Instant::now());
    }
    pub fn remove_tab(&mut self, tab: &str) {
        self.watches.retain(|_, watch| watch.tab != tab);
        self.network_enabled.remove(tab);
    }
    pub fn disconnect(&mut self) {
        self.watches.clear();
        self.network_enabled.clear();
        self.pending.clear();
        self.network_activity.clear();
    }
}
pub(super) fn metadata(id: &str, name: &str) -> Value {
    let known: Value = serde_json::from_str(include_str!("browser_capabilities.json"))
        .expect("validated capability contract data");
    json!({"id":id,"name":name,"type":"cdp","apiSupportOverrides":{"Tab.ax":true,"Tabs.content":true},"capabilities":{"browser":[known["browser"]["visibility"],known["browser"]["viewport"]],"tab":[known["tab"]["cdp"]]}})
}
pub(super) fn documentation(info: &Value) -> Result<Value> {
    let runtime = rquickjs::Runtime::new().map_err(|e| Error::action(e.to_string()))?;
    runtime.set_memory_limit(16 * 1024 * 1024);
    runtime.set_max_stack_size(256 * 1024);
    let deadline = Instant::now() + Duration::from_secs(2);
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let context = rquickjs::Context::full(&runtime).map_err(|e| Error::action(e.to_string()))?;
    context
        .with(|ctx| {
            ctx.eval::<String, _>(format!(
                "({})({},{},{},{},{})",
                include_str!("browser_documentation.js"),
                include_str!("browser_api_manifest.json"),
                include_str!("browser_document_manifest.json"),
                include_str!("browser_docs.json"),
                info,
                include_str!("browser_capabilities.json")
            ))
        })
        .map(|text| json!(text))
        .map_err(|e| Error::action(e.to_string()))
}
fn duration(args: &Value) -> Duration {
    // Retained De normalizes each individual wait stage to at most 3000ms.
    Duration::from_secs_f64(
        args.get("timeoutMs")
            .and_then(Value::as_f64)
            .unwrap_or(3000.0)
            .clamp(0.0, 3000.0)
            / 1000.0,
    )
}
fn wait_options(args: &Value, url_required: bool) -> Result<(Option<String>, Option<String>)> {
    let url = match args.get("url") {
        Some(Value::String(url)) if !url.is_empty() => Some(url.clone()),
        Some(Value::Null) | None if !url_required => None,
        _ => return Err(Error::invalid("playwright_wait_for_url requires a url")),
    };
    let state = if url.is_some() {
        match args["waitUntil"].as_str() {
            None | Some("commit") => None,
            Some(value) => Some(value.to_owned()),
        }
    } else {
        Some(
            args["state"]
                .as_str()
                .or_else(|| args["waitUntil"].as_str())
                .unwrap_or("load")
                .to_owned(),
        )
    };
    if state.as_deref() == Some("networkidle") {
        return Err(Error::action(
            "playwright_wait_for_load_state does not support networkidle",
        ));
    }
    if state
        .as_deref()
        .is_some_and(|value| !matches!(value, "load" | "domcontentloaded"))
    {
        return Err(Error::invalid("Invalid browser load state"));
    }
    Ok((url, state))
}

impl Browser {
    pub(super) fn virtual_paste(&mut self, tab: &str) -> Result<Value> {
        let items = self
            .surface
            .clipboard
            .lock()
            .map_err(|_| Error::action("Clipboard unavailable"))?
            .items
            .clone();
        if items.is_empty() {
            return Err(Error::action("Virtual clipboard is empty"));
        }
        let expression = format!("({})({})", include_str!("browser_paste.js"), json!(items));
        let frame = self.focused_frame(tab)?;
        self.evaluate(
            tab,
            &expression,
            &json!({"frame":frame["id"],"isolated":true}),
        )
    }

    pub(super) fn contract_command(&mut self, method: &str, args: &Value) -> Result<Option<Value>> {
        if matches!(
            method,
            "viewport_set" | "viewport_reset" | "visibility_set" | "visibility_get"
        ) && args.get("tab").is_none()
        {
            if self.extension && method.starts_with("viewport") {
                let value = if method == "viewport_set" {
                    for key in ["width", "height"] {
                        if !args[key].as_u64().is_some_and(|n| n > 0 && n <= 16384) {
                            return Err(Error::invalid(format!("Invalid viewport {key}")));
                        }
                    }
                    json!({"width":args["width"],"height":args["height"]})
                } else {
                    Value::Null
                };
                return self
                    .call("Skyre.setViewport", json!({"value":value}), None)
                    .map(|_| Some(Value::Null));
            }
            let targets = self.call("Target.getTargets", json!({}), None)?;
            let ids: Vec<String> = targets["targetInfos"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|t| t["type"] == "page")
                .filter_map(|t| t["targetId"].as_str().map(str::to_owned))
                .collect();
            if method.starts_with("viewport") {
                if method == "viewport_set" {
                    for key in ["width", "height"] {
                        if !args[key].as_u64().is_some_and(|n| n > 0 && n <= 16384) {
                            return Err(Error::invalid(format!("Invalid viewport {key}")));
                        }
                    }
                }
                for id in ids {
                    let mut selected = args.clone();
                    selected["tab"] = json!(id);
                    self.surface_command(method, &selected)?;
                }
                self.contract.viewport = if method == "viewport_set" {
                    Some(args.clone())
                } else {
                    None
                };
                return Ok(Some(Value::Null));
            }
            let id = ids
                .first()
                .ok_or_else(|| Error::action("No owned browser window is available"))?;
            let mut selected = args.clone();
            selected["tab"] = json!(id);
            return self.surface_command(method, &selected);
        }
        if method == "get_documentation" {
            let name = string(args, "name")?;
            let text = match name {
                "capabilities/tab/cdp" => {
                    "cdp.send(method, params?, {target?, timeoutMs?}?) sends a Chrome DevTools Protocol command. cdp.readEvents({afterSequence?, limit?, methods?, target?, timeoutMs?}?) returns {cursor, events, hasMore, truncated}. A target identifies exactly one sessionId or targetId."
                }
                "capabilities/browser/visibility" => {
                    "visibility.get() returns whether the owned browser window is visible. visibility.set(boolean) changes its minimized state."
                }
                "capabilities/browser/viewport" => {
                    "viewport.set({width,height}) sets positive CSS pixel dimensions for existing and newly created owned tabs. viewport.reset() removes the overrides."
                }
                "capabilities/tab/webmcp" => {
                    "webmcp.fetchTools() snapshots available tools. The returned description() lists them; call(name,input,{timeoutMs?}) invokes a registration from that snapshot."
                }
                _ => {
                    return Err(Error::unsupported(format!(
                        "Documentation is unavailable: {name}"
                    )));
                }
            };
            return Ok(Some(json!(text)));
        }
        if ![
            "navigation_arm",
            "navigation_wait",
            "navigation_poll",
            "navigation_cancel",
            "element_info",
            "element_screenshot",
            "user_history",
            "user_open_tabs",
            "user_claim_tab",
            "wait_for_load_state",
            "wait_for_url",
        ]
        .contains(&method)
        {
            return Ok(None);
        }
        if method == "user_history" || method == "user_open_tabs" || method == "user_claim_tab" {
            let name = match method {
                "user_history" => "Skyre.history",
                "user_open_tabs" => "Skyre.openTabs",
                _ => "Skyre.claimTab",
            };
            return self.call(name, args.clone(), None).map(Some);
        }
        let tab = string(args, "tab")?;
        if method == "navigation_arm" {
            let (url, state) = wait_options(args, false)?;
            self.page(tab)?;
            let initial =
                self.evaluate(tab, "({url:location.href,state:document.readyState})", args)?;
            self.contract.tick();
            if self.contract.watches.len() >= 1024 {
                return Err(Error::action("Too many active navigation expectations"));
            }
            self.contract.next_watch += 1;
            let id = format!("navigation-{}", self.contract.next_watch);
            let timeout = duration(args);
            let mut watch = Watch {
                tab: tab.into(),
                session: self.sessions.get(tab).cloned().unwrap_or_default(),
                frame: self.frame(tab, &json!({}))?["id"]
                    .as_str()
                    .map(str::to_owned),
                owner: self.surface.choosers.owner.clone(),
                waiting_url: url.is_some(),
                url,
                state,
                timeout,
                deadline: Instant::now() + timeout,
                result: None,
                terminal_at: None,
            };
            watch.advance(&initial);
            self.contract.watches.insert(id.clone(), watch);
            return Ok(Some(json!({"id":id})));
        }
        if method == "navigation_cancel" {
            let id = string(args, "watchId")?;
            if self
                .contract
                .watches
                .get(id)
                .is_some_and(|watch| watch.tab != tab || watch.owner != self.surface.choosers.owner)
            {
                return Err(Error::action(
                    "Navigation expectation belongs to another owner",
                ));
            }
            self.contract.watches.remove(id);
            return Ok(Some(Value::Null));
        }
        if method == "navigation_poll" || method == "navigation_wait" {
            let id = string(args, "watchId")?;
            loop {
                let watch = self
                    .contract
                    .watches
                    .get_mut(id)
                    .filter(|watch| watch.tab == tab && watch.owner == self.surface.choosers.owner)
                    .ok_or_else(|| Error::action("Navigation expectation is unavailable"))?;
                watch.expire();
                if let Some(result) = watch.result.clone() {
                    return result.map(|value| {
                        Some(if method == "navigation_poll" {
                            json!({"pending":false,"value":value})
                        } else {
                            value
                        })
                    });
                }
                let current =
                    self.evaluate(tab, "({url:location.href,state:document.readyState})", args)?;
                let watch = self
                    .contract
                    .watches
                    .get_mut(id)
                    .ok_or_else(|| Error::action("Navigation expectation is unavailable"))?;
                watch.advance(&current);
                if let Some(result) = watch.result.clone() {
                    return result.map(|value| {
                        Some(if method == "navigation_poll" {
                            json!({"pending":false,"value":value})
                        } else {
                            value
                        })
                    });
                }
                if method == "navigation_poll" {
                    return Ok(Some(json!({"pending":true})));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        if method == "wait_for_url" || method == "wait_for_load_state" {
            let (url, state) = wait_options(args, method == "wait_for_url")?;
            self.page(tab)?;
            let timeout = duration(args);
            let mut watch = Watch {
                tab: tab.into(),
                session: self.sessions.get(tab).cloned().unwrap_or_default(),
                frame: self.frame(tab, &json!({}))?["id"]
                    .as_str()
                    .map(str::to_owned),
                owner: None,
                waiting_url: url.is_some(),
                url,
                state,
                timeout,
                deadline: Instant::now() + timeout,
                result: None,
                terminal_at: None,
            };
            loop {
                let current =
                    self.evaluate(tab, "({url:location.href,state:document.readyState})", args)?;
                watch.advance(&current);
                if let Some(result) = watch.result {
                    return result.map(Some);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        let x = args["x"]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| Error::invalid("x must be finite"))?;
        let y = args["y"]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| Error::invalid("y must be finite"))?;
        let mut selected = args.clone();
        selected["selector"] = json!({"kind":"point","x":x,"y":y});
        let infos = self.point_infos(tab, &selected)?;
        if method == "element_info" {
            return Ok(Some(infos));
        }
        let captured = self.tab(
            tab,
            "Page.captureScreenshot",
            json!({"format":"png","captureBeyondViewport":false}),
        )?;
        let encoded = base64::engine::general_purpose::STANDARD
            .decode(string(&captured, "data")?)
            .map_err(|_| Error::action("Invalid screenshot encoding"))?;
        let mut image = image::load_from_memory(&encoded)
            .map_err(|e| Error::action(e.to_string()))?
            .to_rgba8();
        let (width, height) = image.dimensions();
        let mut draw = |px: i64, py: i64, color: image::Rgba<u8>| {
            if px >= 0
                && py >= 0
                && (px as u64) < u64::from(width)
                && (py as u64) < u64::from(height)
            {
                image.put_pixel(px as u32, py as u32, color);
            }
        };
        let green = image::Rgba([0, 180, 80, 255]);
        for info in infos.as_array().into_iter().flatten() {
            let b = &info["boundingBox"];
            let l = b["x"].as_f64().unwrap_or(0.) as i64;
            let t = b["y"].as_f64().unwrap_or(0.) as i64;
            let r = l + b["width"].as_f64().unwrap_or(0.) as i64;
            let bottom = t + b["height"].as_f64().unwrap_or(0.) as i64;
            for px in l.max(0)..=r.min(i64::from(width)) {
                for offset in 0..2 {
                    draw(px, t + offset, green);
                    draw(px, bottom - offset, green);
                }
            }
            for py in t.max(0)..=bottom.min(i64::from(height)) {
                for offset in 0..2 {
                    draw(l + offset, py, green);
                    draw(r - offset, py, green);
                }
            }
        }
        for offset in -6..=6 {
            draw(x as i64 + offset, y as i64, image::Rgba([255, 30, 30, 255]));
            draw(x as i64, y as i64 + offset, image::Rgba([255, 30, 30, 255]));
        }
        let mut output = Cursor::new(Vec::new());
        image
            .write_to(&mut output, image::ImageFormat::Png)
            .map_err(|e| Error::action(e.to_string()))?;
        Ok(Some(
            json!({"mime_type":"image/png","data":base64::engine::general_purpose::STANDARD.encode(output.into_inner())}),
        ))
    }
}

// Console messages are returned only when explicitly requested; no runtime logs store them.
pub(super) fn log_entry(event: &Value) -> Option<Value> {
    let p = &event["params"];
    let (level, message, timestamp, url) = match event["method"].as_str()? {
        "Runtime.consoleAPICalled" => {
            let level = match p["type"].as_str()? {
                "warning" => "warn",
                "debug" => "debug",
                "info" => "info",
                "error" | "assert" => "error",
                _ => "log",
            };
            let message = p["args"]
                .as_array()?
                .iter()
                .map(|arg| {
                    arg.get("value")
                        .map(|value| {
                            value
                                .as_str()
                                .map(str::to_owned)
                                .unwrap_or_else(|| value.to_string())
                        })
                        .or_else(|| arg["description"].as_str().map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(" ");
            (
                level,
                message,
                p["timestamp"].as_f64()?,
                p["stackTrace"]["callFrames"][0]["url"].as_str(),
            )
        }
        "Runtime.exceptionThrown" => (
            "error",
            p["exceptionDetails"]["exception"]["description"]
                .as_str()
                .or_else(|| p["exceptionDetails"]["text"].as_str())?
                .to_owned(),
            p["timestamp"].as_f64()?,
            p["exceptionDetails"]["url"].as_str(),
        ),
        "Log.entryAdded" => {
            let e = &p["entry"];
            (
                match e["level"].as_str()? {
                    "warning" => "warn",
                    "error" => "error",
                    "verbose" => "debug",
                    _ => "info",
                },
                e["text"].as_str()?.into(),
                e["timestamp"].as_f64()?,
                e["url"].as_str(),
            )
        }
        _ => return None,
    };
    let runtime = rquickjs::Runtime::new().ok()?;
    let context = rquickjs::Context::full(&runtime).ok()?;
    let timestamp = context
        .with(|ctx| ctx.eval::<String, _>(format!("new Date({timestamp}).toISOString()")))
        .ok()?;
    let mut entry = json!({"level":level,"message":message,"timestamp":timestamp});
    if let Some(url) = url {
        entry["url"] = json!(url);
    }
    Some(entry)
}

#[cfg(test)]
mod navigation_tests {
    use super::*;
    fn oracle() -> Value {
        serde_json::from_str(include_str!("../tests/oracles/browser_navigation.json")).unwrap()
    }
    #[test]
    fn navigation_globs_and_budgets_match_retained_service_oracle() {
        let oracle = oracle();
        for row in oracle["globs"].as_array().unwrap() {
            assert_eq!(
                super::super::surface::url_matches(
                    row["pattern"].as_str().unwrap(),
                    row["value"].as_str().unwrap()
                ),
                row["matches"].as_bool().unwrap(),
                "{row}"
            );
        }
        for row in oracle["durations"].as_array().unwrap() {
            assert_eq!(
                duration(&json!({"timeoutMs":row["value"]})).as_secs_f64() * 1000.0,
                row["milliseconds"].as_f64().unwrap(),
                "{row}"
            );
        }
    }
    #[test]
    fn navigation_event_predicates_preserve_transient_matches_and_session_identity() {
        for row in oracle()["events"].as_array().unwrap() {
            for state in [None, Some("load"), Some("domcontentloaded")] {
                let mut watch = Watch {
                    tab: "7".into(),
                    session: "root-session".into(),
                    frame: Some("root".into()),
                    owner: None,
                    url: state.is_none().then(|| "https://owned.test/end".into()),
                    state: state.map(str::to_owned),
                    waiting_url: state.is_none(),
                    timeout: Duration::from_secs(3),
                    deadline: Instant::now() + Duration::from_secs(3),
                    result: None,
                    terminal_at: None,
                };
                let mut event = row["event"].clone();
                event["sessionId"] = json!("other-session");
                watch.event(&event);
                assert!(watch.result.is_none());
                event["sessionId"] = json!("root-session");
                watch.event(&event);
                let expected = match state {
                    None => !row["url"].is_null(),
                    Some(state) => row[state].as_bool().unwrap(),
                };
                assert_eq!(watch.result.is_some(), expected, "{row}; {state:?}");
                // Later state reads must not discard an event that has already fulfilled the wait.
                watch.advance(&json!({"url":"https://owned.test/other","state":"loading"}));
                assert_eq!(watch.result.is_some(), expected);
            }
        }
    }
    #[test]
    fn navigation_initial_state_and_fresh_load_budget_match_retained_service_oracle() {
        let oracle = oracle();
        for row in oracle["cases"].as_array().unwrap() {
            let spec = &row["spec"];
            let options = wait_options(spec, false);
            if !row["error"].is_null() {
                assert_eq!(options.unwrap_err().message, row["error"]);
                continue;
            }
            let (url, state) = options.unwrap();
            let timeout = duration(spec);
            let mut watch = Watch {
                tab: "7".into(),
                session: "root-session".into(),
                frame: Some("root".into()),
                owner: None,
                waiting_url: url.is_some(),
                url,
                state,
                timeout,
                deadline: Instant::now() + timeout,
                result: None,
                terminal_at: None,
            };
            watch.advance(
                &json!({"url":spec["initial"]["href"],"state":spec["initial"]["readyState"]}),
            );
            let stages = row["calls"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["method"] == "waitForEvent")
                .collect::<Vec<_>>();
            assert_eq!(watch.result.is_some(), stages.is_empty(), "{spec}");
            for stage in stages {
                assert_eq!(
                    watch.timeout.as_millis(),
                    stage["timeoutMs"].as_u64().unwrap() as u128
                );
                let mut expiring = watch.clone();
                expiring.deadline = Instant::now();
                expiring.expire();
                assert_eq!(
                    expiring.result.unwrap().unwrap_err().message,
                    stage["message"]
                );
                if watch.waiting_url {
                    watch.deadline = Instant::now() + Duration::from_millis(1);
                    let before = Instant::now();
                    watch.advance(&json!({"url":spec["url"],"state":"loading"}));
                    assert!(
                        watch.deadline >= before + timeout,
                        "fresh load stage budget"
                    );
                } else {
                    watch.advance(&json!({"url":spec["url"],"state":"complete"}));
                }
            }
            assert!(watch.result.unwrap().is_ok(), "{spec}");
        }
    }
}
