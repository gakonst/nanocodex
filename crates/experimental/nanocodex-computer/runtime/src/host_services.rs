//! Independent local host services. Collection is disabled until explicitly started.
//! The store is isolated from Codex/ChatGPT settings and never sends telemetry.
use crate::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_STORE: u64 = 64 * 1024 * 1024;
const MAX_PAYLOAD: usize = 1024 * 1024;
const EVENT_LIMIT: usize = 1000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Allowed,
    Denied,
    Forbidden,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    pub revision: u64,
    pub default_access: Access,
    pub applications: BTreeMap<String, Access>,
    pub allow_locked_computer: bool,
    pub diagnostics_enabled: bool,
    pub retention: Retention,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            revision: 0,
            default_access: Access::Denied,
            applications: BTreeMap::new(),
            allow_locked_computer: false,
            diagnostics_enabled: false,
            retention: Retention::default(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Retention {
    pub max_records: usize,
    pub max_age_ms: u64,
}
impl Default for Retention {
    fn default() -> Self {
        Self {
            max_records: 1000,
            max_age_ms: 7 * 24 * 3600 * 1000,
        }
    }
}
impl Retention {
    fn validate(&self) -> Result<()> {
        if !(1..=10_000).contains(&self.max_records)
            || !(1000..=365 * 24 * 3600 * 1000).contains(&self.max_age_ms)
        {
            return Err(Error::invalid(
                "Retention must have 1..10000 records and 1000ms..365 days",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub timestamp_ms: u64,
    pub session: String,
    pub kind: String,
    pub payload: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Collector {
    owner: Option<String>,
    running: bool,
    paused: bool,
    include_payload: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub entrypoints: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Plugin {
    versions: BTreeMap<String, PluginManifest>,
    active: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Store {
    schema: u32,
    configuration: Configuration,
    next_record: u64,
    history: Collector,
    history_records: Vec<Record>,
    diagnostics: Vec<Record>,
    plugins: BTreeMap<String, Plugin>,
}
impl Default for Store {
    fn default() -> Self {
        Self {
            schema: 1,
            configuration: Configuration::default(),
            next_record: 1,
            history: Collector::default(),
            history_records: vec![],
            diagnostics: vec![],
            plugins: BTreeMap::new(),
        }
    }
}
struct Lease {
    path: PathBuf,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}
#[derive(Clone)]
struct Subscription {
    owner: String,
    cursor: u64,
    kinds: Vec<String>,
}

pub struct Services {
    root: PathBuf,
    session: String,
    configuration_cache: Option<(Instant, Configuration)>,
    events: VecDeque<Record>,
    next_event: u64,
    next_subscription: u64,
    subscriptions: BTreeMap<String, Subscription>,
}
impl Services {
    pub fn open(root: impl AsRef<Path>, session: &str) -> Result<Self> {
        valid_token(session, "session")?;
        private_directory(root.as_ref())?;
        let root = fs::canonicalize(root)?;
        let mut services = Self {
            root,
            session: session.into(),
            configuration_cache: None,
            events: VecDeque::new(),
            next_event: 1,
            next_subscription: 1,
            subscriptions: BTreeMap::new(),
        };
        let _lease = services.lock()?;
        if !services.root.join("state.json").exists() {
            services.save(&Store::default())?;
        }
        services.load()?;
        services.configuration_cache = None;
        Ok(services)
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn lock(&self) -> Result<Lease> {
        let path = self.root.join(".write-lock");
        let mut file = new_private_file(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Error::new(
                    -32009,
                    "Host store is locked; a crashed writer requires explicit lock recovery",
                )
            } else {
                e.into()
            }
        })?;
        let lease = Lease { path };
        writeln!(file, "{} {}", std::process::id(), self.session)?;
        file.sync_all()?;
        Ok(lease)
    }
    fn load(&self) -> Result<Store> {
        let path = self.root.join("state.json");
        validate_private_file(&path)?;
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_STORE + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STORE {
            return Err(Error::action("Host store exceeds size limit"));
        }
        let store: Store = serde_json::from_slice(&bytes)?;
        if store.schema != 1 {
            return Err(Error::action("Unsupported host store schema"));
        }
        store.configuration.retention.validate()?;
        Ok(store)
    }
    fn save(&self, store: &Store) -> Result<()> {
        let bytes = serde_json::to_vec(store)?;
        if bytes.len() as u64 > MAX_STORE {
            return Err(Error::action("Host store exceeds size limit"));
        }
        atomic_write(&self.root, "state.json", &bytes)
    }
    fn configuration(&mut self, force: bool) -> Result<Configuration> {
        if !force
            && let Some((at, cached)) = &self.configuration_cache
            && at.elapsed() < Duration::from_secs(300)
        {
            return Ok(cached.clone());
        }
        let config = self.load()?.configuration;
        self.configuration_cache = Some((Instant::now(), config.clone()));
        Ok(config)
    }
    /// Lifecycle hook. A record always enters this instance's bounded event ring.
    /// It reaches disk only while history is explicitly running for this session.
    pub fn record(&mut self, kind: &str, payload: Value) -> Result<()> {
        valid_token(kind, "event kind")?;
        bounded_payload(&payload)?;
        let payload = redact(payload);
        let event = Record {
            id: self.next_event,
            timestamp_ms: now_ms(),
            session: self.session.clone(),
            kind: kind.into(),
            payload: payload.clone(),
        };
        self.next_event = self
            .next_event
            .checked_add(1)
            .ok_or_else(|| Error::action("Event sequence exhausted"))?;
        self.events.push_back(event);
        if self.events.len() > EVENT_LIMIT {
            self.events.pop_front();
        }
        let _lease = self.lock()?;
        let mut store = self.load()?;
        if store.history.running
            && !store.history.paused
            && store.history.owner.as_deref() == Some(&self.session)
        {
            let body = if store.history.include_payload {
                payload
            } else {
                Value::Null
            };
            let record = new_record(&mut store, &self.session, kind, body)?;
            store.history_records.push(record);
            retain(&mut store.history_records, &store.configuration.retention);
            self.save(&store)?;
        }
        Ok(())
    }
    /// Called at host/turn shutdown. Only the current session's collector is stopped.
    pub fn end_session(&mut self) -> Result<()> {
        self.record("session.end", Value::Null)?;
        let _lease = self.lock()?;
        let mut store = self.load()?;
        if store.history.owner.as_deref() == Some(&self.session) {
            store.history.running = false;
            store.history.paused = false;
            self.save(&store)?;
        }
        self.subscriptions.clear();
        Ok(())
    }
    pub fn execute(&mut self, method: &str, params: &Value) -> Result<Value> {
        if !params.is_object() {
            return Err(Error::invalid("Host parameters must be an object"));
        }
        match method {
            "host.capabilities" => Ok(
                json!({"storage_schema":1,"history":"explicit start; session owned","events":{"capacity":EVENT_LIMIT,"transport":"cursor polling"},"diagnostics":"opt-in local redacted JSON","plugins":"isolated immutable version directories; no code execution","methods":METHODS}),
            ),
            "host.status" => {
                let store = self.load()?;
                Ok(
                    json!({"session":self.session,"configuration_revision":store.configuration.revision,"history":store.history,"diagnostics_enabled":store.configuration.diagnostics_enabled,"event_subscriptions":self.subscriptions.len(),"root":self.root}),
                )
            }
            "host.configuration.read" => Ok(serde_json::to_value(
                self.configuration(boolean(params, "force", false)?)?,
            )?),
            "host.configuration.patch" => self.patch_configuration(params),
            "host.policy.snapshot" => {
                // Fresh immutable snapshot, never a stale cache for authorization.
                let config = self.configuration(true)?;
                let app = required(params, "app")?;
                Ok(
                    json!({"app":app,"access":config.applications.get(app).unwrap_or(&config.default_access),"configuration_revision":config.revision,"allow_locked_computer":config.allow_locked_computer,"captured_ms":now_ms()}),
                )
            }
            "host.history.start"
            | "host.history.pause"
            | "host.history.resume"
            | "host.history.stop" => self.history_transition(method, params),
            "host.history.status" => Ok(serde_json::to_value(self.load()?.history)?),
            "host.history.append" => {
                let store = self.load()?;
                own_collector(&store.history, &self.session)?;
                if !store.history.running || store.history.paused {
                    return Err(Error::action("History collection is not running"));
                }
                self.record(
                    required(params, "kind")?,
                    params.get("payload").cloned().unwrap_or(Value::Null),
                )?;
                Ok(json!({"recorded":true}))
            }
            "host.history.list" => self.list_records(params, false),
            "host.history.clear" => self.clear_records(params, false),
            "host.events.subscribe" => {
                let kinds = params
                    .get("kinds")
                    .map(|x| serde_json::from_value::<Vec<String>>(x.clone()))
                    .transpose()?
                    .unwrap_or_default();
                for kind in &kinds {
                    valid_token(kind, "event kind")?;
                }
                if self.subscriptions.len() >= 128 {
                    return Err(Error::action("Event subscription limit reached"));
                }
                let cursor = params
                    .get("after")
                    .map(|x| {
                        x.as_u64()
                            .ok_or_else(|| Error::invalid("after must be unsigned"))
                    })
                    .transpose()?
                    .unwrap_or(self.next_event - 1);
                if cursor >= self.next_event {
                    return Err(Error::invalid("Event cursor is in the future"));
                }
                let id = format!("{}-{}", self.session, self.next_subscription);
                self.next_subscription += 1;
                self.subscriptions.insert(
                    id.clone(),
                    Subscription {
                        owner: self.session.clone(),
                        cursor,
                        kinds,
                    },
                );
                Ok(json!({"subscription":id,"cursor":cursor}))
            }
            "host.events.poll" => self.poll_events(params),
            "host.events.unsubscribe" => {
                let id = required(params, "subscription")?;
                if self.subscriptions.remove(id).is_none() {
                    return Err(Error::action("Unknown event subscription"));
                }
                Ok(json!({"removed":true}))
            }
            "host.diagnostics.write" => self.diagnostic(params),
            "host.diagnostics.list" => self.list_records(params, true),
            "host.diagnostics.clear" => self.clear_records(params, true),
            "host.plugins.install" => self.install_plugin(params),
            "host.plugins.list" => Ok(serde_json::to_value(self.load()?.plugins)?),
            "host.plugins.activate" => self.activate_plugin(params),
            "host.plugins.resolve" => self.resolve_plugin(params),
            "host.plugins.uninstall" => self.uninstall_plugin(params),
            _ => Err(Error::unsupported(format!("Unknown host method: {method}"))),
        }
    }
    fn patch_configuration(&mut self, params: &Value) -> Result<Value> {
        let patch = params
            .get("patch")
            .filter(|v| v.is_object())
            .ok_or_else(|| Error::invalid("patch must be an object"))?;
        if patch.get("revision").is_some() {
            return Err(Error::invalid(
                "Configuration revision is controlled by the store",
            ));
        }
        let _lease = self.lock()?;
        let mut store = self.load()?;
        if let Some(expected) = params.get("expected_revision")
            && expected.as_u64() != Some(store.configuration.revision)
        {
            return Err(Error::new(-32009, "Configuration revision conflict"));
        }
        let mut merged = serde_json::to_value(&store.configuration)?;
        merge(&mut merged, patch);
        let mut config: Configuration =
            serde_json::from_value(merged).map_err(|e| Error::invalid(e.to_string()))?;
        config.retention.validate()?;
        for app in config.applications.keys() {
            if app.trim().is_empty() || app.len() > 4096 {
                return Err(Error::invalid("Invalid application policy identity"));
            }
        }
        config.revision = store
            .configuration
            .revision
            .checked_add(1)
            .ok_or_else(|| Error::action("Configuration revision exhausted"))?;
        store.configuration = config.clone();
        retain(&mut store.history_records, &config.retention);
        retain(&mut store.diagnostics, &config.retention);
        self.save(&store)?;
        self.configuration_cache = Some((Instant::now(), config.clone()));
        Ok(serde_json::to_value(config)?)
    }
    fn history_transition(&mut self, method: &str, params: &Value) -> Result<Value> {
        let _lease = self.lock()?;
        let mut store = self.load()?;
        if method == "host.history.start" {
            if store.history.running {
                own_collector(&store.history, &self.session)?;
                return Err(Error::action("History collection is already running"));
            }
            store.history = Collector {
                owner: Some(self.session.clone()),
                running: true,
                paused: false,
                include_payload: boolean(params, "include_payload", false)?,
            };
        } else {
            own_collector(&store.history, &self.session)?;
            if !store.history.running {
                return Err(Error::action("History collection is not running"));
            }
            match method {
                "host.history.pause" => {
                    if store.history.paused {
                        return Err(Error::action("History is already paused"));
                    }
                    store.history.paused = true;
                }
                "host.history.resume" => {
                    if !store.history.paused {
                        return Err(Error::action("History is not paused"));
                    }
                    store.history.paused = false;
                }
                _ => {
                    store.history.running = false;
                    store.history.paused = false;
                }
            }
        }
        self.save(&store)?;
        Ok(serde_json::to_value(store.history)?)
    }
    fn list_records(&self, params: &Value, diagnostics: bool) -> Result<Value> {
        let _lease = self.lock()?;
        let mut store = self.load()?;
        let before = store.history_records.len() + store.diagnostics.len();
        retain(&mut store.history_records, &store.configuration.retention);
        retain(&mut store.diagnostics, &store.configuration.retention);
        if before != store.history_records.len() + store.diagnostics.len() {
            self.save(&store)?;
        }
        let records = if diagnostics {
            &store.diagnostics
        } else {
            &store.history_records
        };
        let after = unsigned(params, "after", 0)?;
        let limit = unsigned(params, "limit", 100)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::invalid("limit must be 1..1000"));
        }
        let rows: Vec<_> = records
            .iter()
            .filter(|r| r.session == self.session && r.id > after)
            .take(limit as usize)
            .collect();
        Ok(json!({"records":rows,"next_cursor":rows.last().map(|r|r.id).unwrap_or(after)}))
    }
    fn clear_records(&self, _params: &Value, diagnostics: bool) -> Result<Value> {
        let _lease = self.lock()?;
        let mut store = self.load()?;
        let records = if diagnostics {
            &mut store.diagnostics
        } else {
            &mut store.history_records
        };
        let before = records.len();
        records.retain(|r| r.session != self.session);
        let removed = before - records.len();
        self.save(&store)?;
        Ok(json!({"removed":removed}))
    }
    fn diagnostic(&mut self, params: &Value) -> Result<Value> {
        let kind = required(params, "kind")?;
        valid_token(kind, "diagnostic kind")?;
        let payload = params.get("payload").cloned().unwrap_or(Value::Null);
        bounded_payload(&payload)?;
        let _lease = self.lock()?;
        let mut store = self.load()?;
        if !store.configuration.diagnostics_enabled {
            return Err(Error::action("Diagnostic persistence is disabled"));
        }
        let record = new_record(&mut store, &self.session, kind, redact(payload))?;
        let id = record.id;
        store.diagnostics.push(record);
        retain(&mut store.diagnostics, &store.configuration.retention);
        self.save(&store)?;
        Ok(json!({"id":id}))
    }
    fn poll_events(&mut self, params: &Value) -> Result<Value> {
        let id = required(params, "subscription")?;
        let limit = unsigned(params, "limit", 100)?;
        if !(1..=1000).contains(&limit) {
            return Err(Error::invalid("limit must be 1..1000"));
        }
        let subscription = self
            .subscriptions
            .get_mut(id)
            .ok_or_else(|| Error::action("Unknown event subscription"))?;
        if subscription.owner != self.session {
            return Err(Error::action("Subscription belongs to another session"));
        }
        let earliest = self.events.front().map(|e| e.id).unwrap_or(self.next_event);
        let lost = earliest.saturating_sub(subscription.cursor.saturating_add(1));
        let mut rows = Vec::new();
        for event in &self.events {
            if event.id <= subscription.cursor {
                continue;
            }
            subscription.cursor = event.id;
            if subscription.kinds.is_empty() || subscription.kinds.contains(&event.kind) {
                rows.push(event.clone());
                if rows.len() >= limit as usize {
                    break;
                }
            }
        }
        Ok(json!({"events":rows,"cursor":subscription.cursor,"lost":lost}))
    }
    fn install_plugin(&mut self, params: &Value) -> Result<Value> {
        let manifest: PluginManifest = serde_json::from_value(
            params
                .get("manifest")
                .cloned()
                .ok_or_else(|| Error::invalid("manifest required"))?,
        )
        .map_err(|e| Error::invalid(e.to_string()))?;
        valid_token(&manifest.id, "plugin id")?;
        valid_token(&manifest.version, "plugin version")?;
        if manifest.files.is_empty() || manifest.files.len() > 1000 {
            return Err(Error::invalid("Plugin must have 1..1000 files"));
        }
        let input = params
            .get("files")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::invalid("files must map relative paths to base64 data"))?;
        if input.len() != manifest.files.len() {
            return Err(Error::invalid(
                "Plugin file set must exactly match manifest",
            ));
        }
        let mut decoded = BTreeMap::new();
        let mut total = 0;
        for (path, hash) in &manifest.files {
            relative_file(path)?;
            let bytes = STANDARD
                .decode(
                    input
                        .get(path)
                        .and_then(Value::as_str)
                        .ok_or_else(|| Error::invalid(format!("Missing file {path}")))?,
                )
                .map_err(|_| Error::invalid("Invalid base64 plugin file"))?;
            total += bytes.len();
            if total > 20 * 1024 * 1024 {
                return Err(Error::invalid("Plugin exceeds 20MiB"));
            }
            if hex(&Sha256::digest(&bytes)) != *hash {
                return Err(Error::invalid(format!("SHA256 mismatch for {path}")));
            }
            decoded.insert(path.clone(), bytes);
        }
        for (kind, path) in &manifest.entrypoints {
            valid_token(kind, "entrypoint kind")?;
            relative_file(path)?;
            if !manifest.files.contains_key(path) {
                return Err(Error::invalid("Entrypoint must refer to an included file"));
            }
        }
        let _lease = self.lock()?;
        let mut store = self.load()?;
        if store
            .plugins
            .get(&manifest.id)
            .is_some_and(|p| p.versions.contains_key(&manifest.version))
        {
            return Err(Error::action(
                "Plugin version is already installed; versions are immutable",
            ));
        }
        let plugins = self.root.join("plugins");
        private_directory(&plugins)?;
        let directory = plugins.join(&manifest.id);
        private_directory(&directory)?;
        let final_path = directory.join(&manifest.version);
        if final_path.exists() {
            return Err(Error::action(
                "Plugin destination already exists outside registry",
            ));
        }
        let stage = directory.join(format!(".staging-{}", unique_id()));
        private_directory(&stage)?;
        let result = (|| -> Result<()> {
            for (path, bytes) in &decoded {
                let destination = stage.join(path);
                let parent = destination.parent().unwrap();
                create_private_tree(&stage, parent)?;
                let mut file = new_private_file(&destination)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            atomic_write(&stage, ".manifest.json", &serde_json::to_vec(&manifest)?)?;
            fs::rename(&stage, &final_path)?;
            sync_directory(&directory)?;
            store
                .plugins
                .entry(manifest.id.clone())
                .or_default()
                .versions
                .insert(manifest.version.clone(), manifest.clone());
            self.save(&store)?;
            Ok(())
        })();
        if result.is_err() && stage.exists() {
            let _ = fs::remove_dir_all(&stage);
        }
        result?;
        Ok(json!({"id":manifest.id,"version":manifest.version,"active":false,"path":final_path}))
    }
    fn activate_plugin(&mut self, params: &Value) -> Result<Value> {
        let id = required(params, "id")?;
        let version = required(params, "version")?;
        let _lease = self.lock()?;
        let mut store = self.load()?;
        let plugin = store
            .plugins
            .get_mut(id)
            .ok_or_else(|| Error::action("Plugin is not installed"))?;
        let manifest = plugin
            .versions
            .get(version)
            .ok_or_else(|| Error::action("Plugin version is not installed"))?;
        verify_plugin(&self.root, manifest)?;
        plugin.active = Some(version.into());
        self.save(&store)?;
        Ok(json!({"id":id,"active":version}))
    }
    fn resolve_plugin(&self, params: &Value) -> Result<Value> {
        let id = required(params, "id")?;
        let store = self.load()?;
        let plugin = store
            .plugins
            .get(id)
            .ok_or_else(|| Error::action("Plugin is not installed"))?;
        let version = plugin
            .active
            .as_ref()
            .ok_or_else(|| Error::action("Plugin has no active version"))?;
        let manifest = &plugin.versions[version];
        verify_plugin(&self.root, manifest)?;
        let base = self.root.join("plugins").join(id).join(version);
        let paths: BTreeMap<_, _> = manifest
            .entrypoints
            .iter()
            .map(|(kind, path)| (kind, base.join(path)))
            .collect();
        Ok(json!({"manifest":manifest,"path":base,"entrypoints":paths}))
    }
    fn uninstall_plugin(&self, params: &Value) -> Result<Value> {
        let id = required(params, "id")?;
        let version = required(params, "version")?;
        valid_token(id, "plugin id")?;
        valid_token(version, "plugin version")?;
        let _lease = self.lock()?;
        let mut store = self.load()?;
        let plugin = store
            .plugins
            .get_mut(id)
            .ok_or_else(|| Error::action("Plugin is not installed"))?;
        if plugin.versions.remove(version).is_none() {
            return Err(Error::action("Plugin version is not installed"));
        }
        if plugin.active.as_deref() == Some(version) {
            plugin.active = None;
        }
        if plugin.versions.is_empty() {
            store.plugins.remove(id);
        }
        // Revoke registry access first. Failure to delete leaves inert recoverable data.
        self.save(&store)?;
        let path = self.root.join("plugins").join(id).join(version);
        if fs::symlink_metadata(&path).is_ok() {
            validate_owned_tree(&self.root, &path)?;
            fs::remove_dir_all(path)?;
        }
        Ok(json!({"removed":true}))
    }
}

pub const METHODS: &[&str] = &[
    "host.capabilities",
    "host.status",
    "host.configuration.read",
    "host.configuration.patch",
    "host.policy.snapshot",
    "host.history.start",
    "host.history.pause",
    "host.history.resume",
    "host.history.stop",
    "host.history.status",
    "host.history.append",
    "host.history.list",
    "host.history.clear",
    "host.events.subscribe",
    "host.events.poll",
    "host.events.unsubscribe",
    "host.diagnostics.write",
    "host.diagnostics.list",
    "host.diagnostics.clear",
    "host.plugins.install",
    "host.plugins.list",
    "host.plugins.activate",
    "host.plugins.resolve",
    "host.plugins.uninstall",
];
fn new_record(store: &mut Store, session: &str, kind: &str, payload: Value) -> Result<Record> {
    let id = store.next_record;
    store.next_record = id
        .checked_add(1)
        .ok_or_else(|| Error::action("Record sequence exhausted"))?;
    Ok(Record {
        id,
        timestamp_ms: now_ms(),
        session: session.into(),
        kind: kind.into(),
        payload,
    })
}
fn retain(records: &mut Vec<Record>, policy: &Retention) {
    let cutoff = now_ms().saturating_sub(policy.max_age_ms);
    records.retain(|r| r.timestamp_ms >= cutoff);
    if records.len() > policy.max_records {
        records.drain(..records.len() - policy.max_records);
    }
}
fn own_collector(state: &Collector, session: &str) -> Result<()> {
    if state.owner.as_deref() != Some(session) {
        return Err(Error::action(
            "History collector belongs to another session",
        ));
    }
    Ok(())
}
fn bounded_payload(payload: &Value) -> Result<()> {
    if serde_json::to_vec(payload)?.len() > MAX_PAYLOAD {
        return Err(Error::invalid("Payload exceeds 1MiB"));
    }
    Ok(())
}
/// Structural redaction only; freeform strings may still contain sensitive content.
fn redact(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let normalized = k.to_ascii_lowercase().replace(['-', '_'], "");
                    let secret = [
                        "password",
                        "passwd",
                        "secret",
                        "token",
                        "credential",
                        "authorization",
                        "cookie",
                        "apikey",
                        "privatekey",
                    ]
                    .iter()
                    .any(|word| normalized.contains(word));
                    (
                        k,
                        if secret {
                            json!("[REDACTED]")
                        } else {
                            redact(v)
                        },
                    )
                })
                .collect(),
        ),
        Value::Array(xs) => Value::Array(xs.into_iter().map(redact).collect()),
        v => v,
    }
}
fn merge(target: &mut Value, patch: &Value) {
    if let (Some(dst), Some(src)) = (target.as_object_mut(), patch.as_object()) {
        for (k, v) in src {
            if v.is_null() {
                dst.remove(k);
            } else {
                merge(dst.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
    } else {
        *target = patch.clone();
    }
}
fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::invalid(format!("{key} must be a non-empty string")))
}
fn boolean(params: &Value, key: &str, default: bool) -> Result<bool> {
    params
        .get(key)
        .map(|v| {
            v.as_bool()
                .ok_or_else(|| Error::invalid(format!("{key} must be Boolean")))
        })
        .transpose()
        .map(|v| v.unwrap_or(default))
}
fn unsigned(params: &Value, key: &str, default: u64) -> Result<u64> {
    params
        .get(key)
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| Error::invalid(format!("{key} must be an unsigned integer")))
        })
        .transpose()
        .map(|v| v.unwrap_or(default))
}
fn valid_token(token: &str, what: &str) -> Result<()> {
    if token.is_empty()
        || token.len() > 128
        || token == "."
        || token == ".."
        || !token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
    {
        return Err(Error::invalid(format!(
            "Invalid {what}; use 1..128 letters, digits, '.', '_' or '-'"
        )));
    }
    Ok(())
}
fn relative_file(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 1024
        || path.contains('\\')
        || path.contains(':')
        || path.starts_with('.')
        || Path::new(path)
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(Error::invalid(
            "Plugin paths must be safe relative file paths",
        ));
    }
    for p in path.split('/') {
        if p.is_empty() || p == "." || p == ".." || p.starts_with('.') {
            return Err(Error::invalid(
                "Plugin paths cannot contain dot or hidden components",
            ));
        }
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn unique_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        now_ms(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn new_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
fn validate_private_file(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::action(
            "Host store file must be a regular non-symlink file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(Error::action(
                "Host store file must be owned by this user with private permissions",
            ));
        }
    }
    Ok(())
}
fn validate_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(Error::action("Host store directory cannot be a symlink"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(Error::action(
                "Host store directory must be owned by this user with private permissions",
            ));
        }
    }
    Ok(())
}
fn private_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    validate_directory(path)
}
fn create_private_tree(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| Error::invalid("Path escaped plugin stage"))?;
    let mut current = root.to_path_buf();
    for c in relative.components() {
        current.push(c);
        private_directory(&current)?;
    }
    Ok(())
}
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}
fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = root.join(format!(".write-{}", unique_id()));
    let result = (|| -> Result<()> {
        let mut file = new_private_file(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, root.join(name))?;
        sync_directory(root)
    })();
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}
fn verify_plugin(root: &Path, manifest: &PluginManifest) -> Result<()> {
    let directory = root
        .join("plugins")
        .join(&manifest.id)
        .join(&manifest.version);
    validate_owned_tree(root, &directory)?;
    for (path, expected) in &manifest.files {
        relative_file(path)?;
        let destination = directory.join(path);
        let mut parent = destination.parent();
        while let Some(p) = parent {
            if p == directory {
                break;
            }
            validate_directory(p)?;
            parent = p.parent();
        }
        validate_private_file(&destination)?;
        let mut bytes = Vec::new();
        File::open(destination)?
            .take(20 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 20 * 1024 * 1024 || hex(&Sha256::digest(&bytes)) != *expected {
            return Err(Error::action(format!("Plugin integrity failure: {path}")));
        }
    }
    Ok(())
}

fn validate_owned_tree(root: &Path, directory: &Path) -> Result<()> {
    let relative = directory
        .strip_prefix(root)
        .map_err(|_| Error::action("Owned path escaped store"))?;
    let mut current = root.to_path_buf();
    validate_directory(&current)?;
    for part in relative.components() {
        current.push(part);
        validate_directory(&current)?;
    }
    Ok(())
}
