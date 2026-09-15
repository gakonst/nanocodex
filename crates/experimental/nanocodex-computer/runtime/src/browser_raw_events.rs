//! Per-tab raw debugger records. Queries only inspect captured identity; they
//! never resolve a target, attach a debugger, or consult mutable routing state.
use crate::{Error, Result};
use serde_json::{Value, json};
use std::collections::{BTreeMap, VecDeque};
#[path = "browser_raw_wait.rs"]
pub(super) mod waiting;
const PER_TAB: usize = 1000;
const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_TABS: usize = 1024;
#[derive(Clone, Debug)]
pub(super) enum Selector {
    SessionId(String),
    TargetId(String),
    Unmatchable,
}
#[derive(Clone, Debug, Default)]
pub(super) struct Query {
    pub after: Option<f64>,
    pub methods: Option<Vec<String>>,
    pub target: Option<Selector>,
    pub limit: Option<usize>,
}
impl Query {
    fn utf16(value: &Value) -> Result<Option<String>> {
        let units = value
            .as_array()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::invalid("CDP UTF-16 filter must contain code units"))?;
        let units = units
            .iter()
            .map(|v| {
                v.as_u64()
                    .and_then(|v| u16::try_from(v).ok())
                    .ok_or_else(|| Error::invalid("Invalid CDP UTF-16 code unit"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(String::from_utf16(&units).ok())
    }
    /// The established trusted native driver admits limit zero. The public
    /// facade applies the original schema's positive limit before dispatch.
    pub fn native(args: &Value) -> Result<Self> {
        let number = |name: &str| -> Result<Option<f64>> {
            let Some(value) = args.get(name).filter(|v| !v.is_null()) else {
                return Ok(None);
            };
            let number = value
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.0 && v.fract() == 0.0)
                .ok_or_else(|| Error::invalid(format!("{name} must be a nonnegative integer")))?;
            Ok(Some(number))
        };
        let after = number("after_sequence")?;
        // Larger trusted native integer limits select the same bounded ring.
        // The public facade separately enforces the original maximum of 1000.
        let limit = number("limit")?.map(|n| n.min(PER_TAB as f64) as usize);
        let mut methods =
            args.get("methods")
                .filter(|v| !v.is_null())
                .map(|value| {
                    let methods = value.as_array().filter(|v| !v.is_empty()).ok_or_else(|| {
                        Error::invalid("CDP methods must be a nonempty string array")
                    })?;
                    methods
                        .iter()
                        .map(|v| {
                            v.as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                                .ok_or_else(|| {
                                    Error::invalid("CDP methods must contain nonempty strings")
                                })
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
        let mut target = args
            .get("target")
            .filter(|v| !v.is_null())
            .map(|target| {
                let string = |name: &str| -> Result<Option<String>> {
                    target
                        .get(name)
                        .map(|v| {
                            v.as_str()
                                .filter(|s| !s.is_empty())
                                .map(str::to_owned)
                                .ok_or_else(|| {
                                    Error::invalid(
                                        "CDP target identifiers must be nonempty strings",
                                    )
                                })
                        })
                        .transpose()
                };
                match (string("sessionId")?, string("targetId")?) {
                    (Some(session), None) => Ok(Selector::SessionId(session)),
                    (None, Some(target)) => Ok(Selector::TargetId(target)),
                    _ => Err(Error::invalid(
                        "CDP target requires exactly one sessionId or targetId",
                    )),
                }
            })
            .transpose()?;
        // This is query data, never a route or a grant. The facade uses
        // unit arrays only where JSON's scalar-string decoder would lose an
        // admitted JavaScript UTF-16 filter. A lone surrogate cannot equal any
        // retained UTF-8 source string, but valid members in a mixed set remain.
        if let Some(encoded) = args.get("filter_utf16") {
            let encoded = encoded
                .as_object()
                .ok_or_else(|| Error::invalid("CDP UTF-16 filters must be an object"))?;
            if encoded
                .keys()
                .any(|key| key != "methods" && key != "target")
            {
                return Err(Error::invalid("Unknown CDP UTF-16 filter"));
            }
            if let Some(value) = encoded.get("methods") {
                if args.get("methods").is_some_and(|v| !v.is_null()) {
                    return Err(Error::invalid("Conflicting CDP method filters"));
                }
                let values = value
                    .as_array()
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| Error::invalid("CDP UTF-16 methods must be a nonempty array"))?;
                methods = Some(
                    values
                        .iter()
                        .map(Self::utf16)
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                        .flatten()
                        .collect(),
                );
            }
            if let Some(value) = encoded.get("target") {
                if args.get("target").is_some_and(|v| !v.is_null()) {
                    return Err(Error::invalid("Conflicting CDP target filters"));
                }
                let value = value
                    .as_object()
                    .filter(|v| v.len() == 1)
                    .ok_or_else(|| Error::invalid("CDP UTF-16 target requires one identifier"))?;
                let (kind, units) = value.iter().next().unwrap();
                if !["sessionId", "targetId"].contains(&kind.as_str()) {
                    return Err(Error::invalid("Invalid CDP UTF-16 target kind"));
                }
                target = Some(match Self::utf16(units)? {
                    None => Selector::Unmatchable,
                    Some(text) if kind == "sessionId" => Selector::SessionId(text),
                    Some(text) => Selector::TargetId(text),
                });
            }
        }
        Ok(Self {
            after,
            methods,
            target,
            limit,
        })
    }
}
struct Record {
    sequence: u64,
    source: Value,
    method: String,
    params: Option<Value>,
    tracked_target: Option<String>,
    order: u64,
    bytes: usize,
}
impl Record {
    fn public(&self) -> Value {
        let mut result =
            json!({"sequence":self.sequence,"source":self.source,"method":self.method});
        if let Some(params) = &self.params {
            result["params"] = params.clone();
        }
        result
    }
    fn matches(&self, query: &Query, after: f64) -> bool {
        self.sequence as f64 > after
            && query
                .methods
                .as_ref()
                .is_none_or(|methods| methods.contains(&self.method))
            && match &query.target {
                None => true,
                Some(Selector::Unmatchable) => false,
                Some(Selector::SessionId(session)) => {
                    self.source["sessionId"].as_str() == Some(session)
                }
                Some(Selector::TargetId(target)) => {
                    self.source["targetId"].as_str() == Some(target)
                        || self.tracked_target.as_ref() == Some(target)
                }
            }
    }
}
#[derive(Default)]
struct Tab {
    sequence: u64,
    evicted: u64,
    records: VecDeque<Record>,
}
pub(super) struct Source {
    pub tab: String,
    pub source: Value,
    pub tracked_target: Option<String>,
    pub top_level: bool,
}
#[derive(Default)]
pub(super) struct Context {
    pub source: Option<Source>,
    pub discard: Option<String>,
}
#[derive(Default)]
pub(super) struct Log {
    tabs: BTreeMap<String, Tab>,
    bytes: usize,
    order: u64,
    origins: BTreeMap<String, String>,
    attachment_sequence: u64,
    attachments: BTreeMap<String, (u64, String)>,
    pub(super) waits: waiting::State,
}
impl Log {
    pub fn attach(&mut self, tab: &str, session: &str) -> Result<()> {
        self.ensure_tab(tab)?;
        self.attachment_sequence = self
            .attachment_sequence
            .checked_add(1)
            .ok_or_else(|| Error::action("Raw event attachment identity exhausted"))?;
        self.attachments
            .insert(tab.into(), (self.attachment_sequence, session.into()));
        Ok(())
    }
    pub fn attachment(&self, tab: &str) -> Option<(u64, String)> {
        self.attachments.get(tab).cloned()
    }
    pub fn with_waits<T>(&mut self, f: impl FnOnce(&mut waiting::State, &Self) -> T) -> T {
        let mut waits = std::mem::take(&mut self.waits);
        let value = f(&mut waits, self);
        self.waits = waits;
        value
    }
    pub fn ensure_tab(&mut self, tab: &str) -> Result<()> {
        if !self.tabs.contains_key(tab) && self.tabs.len() >= MAX_TABS {
            return Err(Error::action("Raw event tab limit exceeded"));
        }
        self.tabs.entry(tab.into()).or_default();
        Ok(())
    }
    pub fn event(&mut self, context: Context, event: &Value, internal: bool) -> Result<()> {
        if let Some(owner) = context.source
            && !internal
            && let Some(method) = event["method"].as_str().filter(|s| !s.is_empty())
        {
            self.record(
                &owner.tab,
                owner.source,
                owner.tracked_target,
                method,
                event.get("params").cloned(),
            )?;
            if owner.top_level
                && method == "Page.frameNavigated"
                && event["params"]["frame"]["parentId"].is_null()
                && let Some(origin) = event["params"]["frame"]["securityOrigin"].as_str()
            {
                if self.origins.get(&owner.tab).is_none_or(|old| old != origin) {
                    self.discard(&owner.tab);
                }
                self.origins.insert(owner.tab, origin.to_owned());
            }
        }
        if let Some(tab) = context.discard {
            self.remove(&tab);
        }
        self.with_waits(|waits, log| waits.event(|tab, after, query| log.read(tab, after, query)));
        Ok(())
    }
    pub fn remove(&mut self, tab: &str) {
        self.discard(tab);
        self.origins.remove(tab);
        self.with_waits(|waits, log| {
            waits.detach(tab, |tab, after, query| log.read(tab, after, query))
        });
        self.attachments.remove(tab);
    }
    pub fn cursor(&self, tab: &str) -> u64 {
        self.tabs.get(tab).map_or(0, |tab| tab.sequence)
    }
    pub fn record(
        &mut self,
        tab: &str,
        source: Value,
        tracked_target: Option<String>,
        method: &str,
        params: Option<Value>,
    ) -> Result<()> {
        self.ensure_tab(tab)?;
        self.order = self
            .order
            .checked_add(1)
            .ok_or_else(|| Error::action("Raw event order exhausted"))?;
        let current = self.tabs.get_mut(tab).unwrap();
        // The captured owner uses JavaScript Number increment semantics.
        current.sequence = (current.sequence as f64 + 1.0) as u64;
        let mut record = Record {
            sequence: current.sequence,
            source,
            method: method.into(),
            params: params.filter(|v| !v.is_null()),
            tracked_target,
            order: self.order,
            bytes: 0,
        };
        record.bytes = record.public().to_string().len()
            + record.tracked_target.as_ref().map_or(0, String::len);
        if record.bytes > MAX_BYTES {
            current.evicted = current.sequence;
            return Ok(());
        }
        self.bytes += record.bytes;
        current.records.push_back(record);
        while current.records.len() > PER_TAB {
            let old = current.records.pop_front().unwrap();
            self.bytes -= old.bytes;
            current.evicted = current.evicted.max(old.sequence);
        }
        // Independent transport memory bound. Byte eviction is explicitly a
        // retention divergence; query algebra remains the same over retained data.
        while self.bytes > MAX_BYTES {
            let owner = self
                .tabs
                .iter()
                .filter_map(|(tab, log)| log.records.front().map(|record| (tab, record.order)))
                .min_by_key(|(_, order)| *order)
                .map(|(tab, _)| tab.clone())
                .unwrap();
            let log = self.tabs.get_mut(&owner).unwrap();
            let old = log.records.pop_front().unwrap();
            self.bytes -= old.bytes;
            log.evicted = log.evicted.max(old.sequence);
        }
        Ok(())
    }
    pub fn discard(&mut self, tab: &str) {
        if let Some(log) = self.tabs.get_mut(tab) {
            log.evicted = log.evicted.max(log.sequence);
            for record in log.records.drain(..) {
                self.bytes -= record.bytes;
            }
        }
    }
    pub fn disconnect(&mut self) {
        // Connection loss revokes even already frozen packets. It never
        // converts an old operation into a new connection's read authority.
        self.waits.clear();
        for tab in self.tabs.keys().cloned().collect::<Vec<_>>() {
            self.remove(&tab);
        }
    }
    pub fn read(&self, tab: &str, after: f64, query: &Query) -> Value {
        let Some(log) = self.tabs.get(tab) else {
            return json!({"cursor":0,"events":[],"hasMore":false,"truncated":false});
        };
        let matching = log
            .records
            .iter()
            .filter(|record| record.matches(query, after));
        let total = matching.clone().count();
        let selected = matching
            .take(query.limit.unwrap_or(total))
            .collect::<Vec<_>>();
        let has_more = total > selected.len();
        let cursor = if has_more {
            selected
                .last()
                .map_or(log.sequence, |record| record.sequence)
        } else {
            log.sequence
        };
        json!({"cursor":cursor,"events":selected.iter().map(|record|record.public()).collect::<Vec<_>>(),"hasMore":has_more,"truncated":after<(log.evicted as f64)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn query(value: &Value) -> Query {
        Query {
            after: None,
            methods: value["methods"]
                .as_array()
                .map(|v| v.iter().map(|s| s.as_str().unwrap().into()).collect()),
            limit: value["limit"].as_u64().map(|v| v as usize),
            target: value.get("target").map(|t| {
                if let Some(s) = t["sessionId"].as_str() {
                    Selector::SessionId(s.into())
                } else {
                    Selector::TargetId(t["targetId"].as_str().unwrap().into())
                }
            }),
        }
    }
    #[test]
    fn raw_event_queries_match_pinned_original_full_retained_record_matrix() {
        let original: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_raw_events.json")).unwrap();
        for case in original["scenarios"].as_array().unwrap() {
            let mut log = Log::default();
            let mut tracked = BTreeMap::new();
            for action in case["actions"].as_array().unwrap() {
                let tab = action["tab"].to_string();
                match action["op"].as_str().unwrap() {
                    "track" => {
                        tracked.insert(
                            action["session"].as_str().unwrap().to_owned(),
                            action["target"].as_str().unwrap().to_owned(),
                        );
                    }
                    "discard" => log.discard(&tab),
                    "record" => {
                        let event = &action["event"];
                        let source = &event["source"];
                        let target = source["targetId"].as_str().map(str::to_owned).or_else(|| {
                            source["sessionId"]
                                .as_str()
                                .and_then(|session| tracked.get(session))
                                .cloned()
                        });
                        log.record(
                            &tab,
                            source.clone(),
                            target,
                            event["method"].as_str().unwrap(),
                            event.get("params").cloned(),
                        )
                        .unwrap();
                    }
                    _ => panic!("unknown oracle action"),
                }
            }
            for row in case["rows"].as_array().unwrap() {
                assert_eq!(
                    log.read(
                        &row["tab"].to_string(),
                        row["after"].as_f64().unwrap(),
                        &query(&row["query"])
                    ),
                    row["result"],
                    "{} {row}",
                    case["name"]
                );
            }
        }
    }
    #[test]
    fn raw_event_utf16_filters_match_original_over_scalar_records() {
        let original: Value =
            serde_json::from_str(include_str!("../tests/oracles/browser_raw_events.json")).unwrap();
        let mut log = Log::default();
        for (index, method) in ["A", "\u{10000}"].into_iter().enumerate() {
            log.record(
                "1",
                json!({"sessionId":"root-one","targetId":"one"}),
                Some("one".into()),
                method,
                Some(json!({"i":index+1})),
            )
            .unwrap();
        }
        for row in original["utf16"].as_array().unwrap() {
            let units = row["units"].clone();
            let encoded = if row["kind"] == "methods" {
                json!({"methods":if row["mixed"]==true {vec![json!([65]),units]}else{vec![units]}})
            } else {
                json!({"target":{row["kind"].as_str().unwrap():units}})
            };
            let query = Query::native(&json!({"after_sequence":0,"filter_utf16":encoded})).unwrap();
            assert_eq!(log.read("1", 0., &query), row["result"], "{row}");
        }
        for invalid in [
            json!(null),
            json!([]),
            json!({"unknown":[]}),
            json!({"methods":[]}),
            json!({"methods":[[]]}),
            json!({"methods":[[65536]]}),
            json!({"methods":[[-1]]}),
            json!({"methods":[[65.5]]}),
            json!({"methods":[[true]]}),
            json!({"methods":[["65"]]}),
            json!({"target":{"sessionId":[65],"targetId":[65]}}),
            json!({"target":{"unknown":[65]}}),
        ] {
            assert!(
                Query::native(&json!({"filter_utf16":invalid})).is_err(),
                "{invalid}"
            );
        }
        assert!(
            Query::native(&json!({"methods":["A"],"filter_utf16":{"methods":[[65]]}})).is_err()
        );
        assert!(Query::native(&json!({"target":{"sessionId":"root-one"},"filter_utf16":{"target":{"sessionId":[65]}}})).is_err());
    }
    #[test]
    fn raw_event_bounded_retention_keeps_monotonic_loss_watermarks() {
        let mut log = Log::default();
        for i in 0..10 {
            log.record(
                "tab",
                json!({"sessionId":"root"}),
                None,
                "Owned.data",
                Some(json!({"i":i})),
            )
            .unwrap();
        }
        log.record(
            "tab",
            json!({}),
            None,
            "Owned.tooLarge",
            Some(json!({"text":"x".repeat(MAX_BYTES)})),
        )
        .unwrap();
        for _ in 0..1000 {
            log.record("tab", json!({}), None, "Owned.next", None)
                .unwrap();
        }
        assert_eq!(log.tabs["tab"].evicted, 11);
        assert_eq!(log.read("tab", 10., &Query::default())["truncated"], true);
        assert_eq!(log.read("tab", 11., &Query::default())["truncated"], false);
        for tab in ["a", "b", "c"] {
            log.record(
                tab,
                json!({}),
                None,
                "Owned.bytes",
                Some(json!({"text":"x".repeat(MAX_BYTES/2)})),
            )
            .unwrap();
        }
        assert!(log.bytes <= MAX_BYTES);
        log.disconnect();
        assert_eq!(log.bytes, 0);
        assert_eq!(log.cursor("tab"), 1011);
        assert_eq!(
            log.read("tab", 1011., &Query::default())["truncated"],
            false
        );
    }
    #[test]
    fn raw_event_receipt_uses_captured_active_identity_and_root_lifecycle() {
        let mut routes = super::super::dialog::State::default();
        let mut log = Log::default();
        routes.root("tab", "root");
        routes.child("tab", "child", "old-target", "root");
        let event = json!({"sessionId":"child","method":"Owned.event","params":{"value":1}});
        let context = routes.raw_event_context(&event);
        routes.child("tab", "child", "new-target", "root");
        log.event(context, &event, false).unwrap();
        assert_eq!(
            log.read(
                "tab",
                0.,
                &Query {
                    target: Some(Selector::TargetId("old-target".into())),
                    ..Default::default()
                }
            )["events"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        routes.remove_session("tab", "child");
        assert!(routes.raw_event_context(&event).source.is_none());
        for origin in ["http://owned-a", "http://owned-b"] {
            let event = json!({"sessionId":"root","method":"Page.frameNavigated","params":{"frame":{"securityOrigin":origin}}});
            log.event(routes.raw_event_context(&event), &event, false)
                .unwrap();
        }
        assert_eq!(
            log.read("tab", 0., &Query::default()),
            json!({"cursor":3,"events":[],"hasMore":false,"truncated":true})
        );
        let detach = json!({"method":"Target.detachedFromTarget","params":{"sessionId":"root"}});
        let context = routes.raw_event_context(&detach);
        routes.event(&detach);
        log.event(context, &detach, false).unwrap();
        assert!(routes.raw_event_context(&event).source.is_none());
        assert_eq!(log.cursor("tab"), 3);
    }
}
