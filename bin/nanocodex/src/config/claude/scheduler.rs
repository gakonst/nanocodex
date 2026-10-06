//! Session-local scheduling: the TUI owns the clock and dispatches only at idle.
//! Advancing/removing a due task is persisted before dispatch. A crash in that
//! gap may lose a fire; this is deliberately at-most-once admission, not an
//! exactly-once provider delivery guarantee. No daemon survives the CLI.
use chrono::{Datelike, Local, LocalResult, TimeZone, Timelike, Utc};
use fs2::FileExt;
use nanocodex::claude::{ClaudeTools, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const MAX_TASKS: usize = 50;
const MAX_RECEIPTS: usize = 4096;
const WEEK: i64 = 7 * 86400;
const MAX_BYTES: u64 = 4 * 1024 * 1024;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Serialize, Deserialize)]
struct Task {
    id: String,
    cron: Option<String>,
    timezone: String,
    prompt: String,
    recurring: bool,
    created_at: i64,
    expires_at: i64,
    next_fire_at: i64,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    jitter_seconds: i64,
    #[serde(default)]
    noop_streak: u32,
}
#[derive(Default, Serialize, Deserialize)]
struct Journal {
    session: String,
    #[serde(default)]
    owner_epoch: String,
    tasks: BTreeMap<String, Task>,
    receipts: BTreeMap<String, Receipt>,
    #[serde(default)]
    claims: Vec<Value>,
    #[serde(default)]
    wakeup_started: Option<i64>,
    #[serde(default)]
    wakeup_prompt: Option<String>,
    #[serde(default)]
    wakeup_iteration_active: bool,
    #[serde(default)]
    wakeup_iteration_token: Option<String>,
    #[serde(default)]
    wakeup_fallback_used: bool,
    #[serde(default)]
    noop_streak: u32,
}
#[derive(Serialize, Deserialize)]
struct Receipt {
    fingerprint: String,
    output: Value,
}

pub(crate) struct DuePrompt {
    pub(crate) id: String,
    pub(crate) prompt: String,
    pub(crate) iteration_token: Option<String>,
}
pub(crate) struct SessionScheduler {
    directory: PathBuf,
    owner_epoch: String,
    owned_sessions: Mutex<BTreeSet<String>>,
    pending: Mutex<VecDeque<(String, DuePrompt)>>,
}
impl SessionScheduler {
    pub(crate) fn new(home: PathBuf) -> Self {
        Self {
            directory: home.join("claude/schedules"),
            owner_epoch: uuid::Uuid::new_v4().to_string(),
            owned_sessions: Mutex::new(BTreeSet::new()),
            pending: Mutex::new(VecDeque::new()),
        }
    }
    /// Bounded host-only ingress for Monitor or other native notifications.
    /// Producers retain their owner session; a hidden branch never receives a
    /// prompt intended for a different retained handle. No model send bridge.
    pub(crate) fn enqueue(&self, session: String, id: String, prompt: String) -> Result<()> {
        validate_text(&prompt, "automatic prompt", 16384)?;
        let mut queue = self
            .pending
            .lock()
            .map_err(|_| "automatic prompt queue poisoned")?;
        if queue.len() >= 50 {
            return Err("automatic prompt queue is full (50)".into());
        }
        queue.push_back((
            session,
            DuePrompt {
                id,
                prompt,
                iteration_token: None,
            },
        ));
        Ok(())
    }
    pub(crate) fn enabled(tui: bool) -> bool {
        tui && std::env::var("CLAUDE_CODE_DISABLE_CRON").as_deref() != Ok("1")
    }
    fn transact<T>(&self, session: &str, f: impl FnOnce(&mut Journal) -> Result<T>) -> Result<T> {
        self.transact_owned(session, false, f)
    }
    fn transact_owned<T>(
        &self,
        session: &str,
        adopt: bool,
        f: impl FnOnce(&mut Journal) -> Result<T>,
    ) -> Result<T> {
        fs::create_dir_all(&self.directory).map_err(|e| e.to_string())?;
        let path = self
            .directory
            .join(format!("{}.json", hex::encode(session)));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.lock_exclusive().map_err(|e| e.to_string())?;
        let mut journal: Journal = match fs::metadata(&path) {
            Ok(meta) => {
                if meta.len() > MAX_BYTES {
                    return Err("scheduler journal exceeds 4 MiB".into());
                }
                serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("invalid scheduler journal: {e}"))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Journal {
                session: session.into(),
                ..Journal::default()
            },
            Err(e) => return Err(e.to_string()),
        };
        if journal.session != session {
            return Err("scheduler session mismatch".into());
        }
        if !adopt && journal.owner_epoch != self.owner_epoch {
            return Err(
                "scheduler ownership changed; this CLI is fenced by a newer session owner".into(),
            );
        }
        let before = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
        if adopt {
            journal.owner_epoch.clone_from(&self.owner_epoch);
        }
        let output = f(&mut journal)?;
        let after = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
        if before != after {
            if after.len() as u64 > MAX_BYTES {
                return Err("scheduler journal exceeds 4 MiB".into());
            }
            let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            file.write_all(&after)
                .and_then(|()| file.sync_all())
                .map_err(|e| e.to_string())?;
            fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            File::open(&self.directory)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        Ok(output)
    }
    /// Reopen policy: discard elapsed one-shots and dynamic wakeups; recurring
    /// schedules resume at their next future match without a backlog.
    pub(crate) fn resume(&self, session: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        self.transact_owned(session, true, |journal| {
            clear_wakeup(journal);
            journal.tasks.retain(|_, task| {
                task.cron.is_some()
                    && task.expires_at > now
                    && (task.recurring || task.next_fire_at > now)
            });
            for task in journal.tasks.values_mut() {
                if task.next_fire_at <= now {
                    task.next_fire_at = Cron::parse(task.cron.as_deref().ok_or("missing cron")?)?
                        .next(now, &task.timezone)?
                        + task.jitter_seconds;
                }
            }
            Ok(())
        })?;
        self.owned_sessions
            .lock()
            .map_err(|_| "scheduler owner set poisoned")?
            .insert(session.into());
        Ok(())
    }
    /// Called only after the UI has verified idle and no queued user turn.
    pub(crate) fn take_due(&self, session: &str) -> Result<Option<DuePrompt>> {
        if !self.owns(session)? {
            return Ok(None);
        }
        // Check ownership even for Monitor ingress before removing its event.
        self.transact(session, |_| Ok(()))?;
        {
            let mut queue = self
                .pending
                .lock()
                .map_err(|_| "automatic prompt queue poisoned")?;
            if let Some(index) = queue.iter().position(|(owner, _)| owner == session) {
                return Ok(queue.remove(index).map(|(_, prompt)| prompt));
            }
        }
        let now = Utc::now().timestamp();
        self.transact(session, |journal| {
            journal.tasks.retain(|_,t| t.recurring || t.id == "wakeup" || t.expires_at > now);
            let selected = journal.tasks.values().filter(|t| t.next_fire_at <= now || ((t.recurring || t.id == "wakeup") && t.expires_at <= now))
                .min_by_key(|t| (t.next_fire_at, &t.id)).cloned();
            let Some(task) = selected else { return Ok(None); };
            if task.recurring && now < task.expires_at {
                let next = Cron::parse(task.cron.as_deref().ok_or("missing cron")?)?.next(now, &task.timezone)? + task.jitter_seconds;
                journal.tasks.get_mut(&task.id).ok_or("task disappeared")?.next_fire_at = next;
            } else { journal.tasks.remove(&task.id); }
            if task.id == "wakeup" {
                journal.wakeup_iteration_active = true;
                journal.wakeup_iteration_token = Some(uuid::Uuid::new_v4().to_string());
                journal.wakeup_prompt = Some(task.prompt.clone());
            }
            journal.claims.push(json!({"id":task.id,"scheduled_at":task.next_fire_at,"consumed_at":now,"delivery":"consumed_before_dispatch"}));
            if journal.claims.len() > 512 { journal.claims.remove(0); }
            Ok(Some(DuePrompt { iteration_token: if task.id == "wakeup" { journal.wakeup_iteration_token.clone() } else { None }, id: task.id, prompt: task.prompt }))
        })
    }
    fn owns(&self, session: &str) -> Result<bool> {
        Ok(self
            .owned_sessions
            .lock()
            .map_err(|_| "scheduler owner set poisoned")?
            .contains(session))
    }
    pub(crate) fn stop_wakeup(&self, session: &str) -> Result<()> {
        if !self.owns(session)? {
            return Ok(());
        }
        self.transact(session, |j| {
            clear_wakeup(j);
            Ok(())
        })
    }
    /// Start a user-requested dynamic loop. Call only for its initial turn;
    /// take_due marks subsequent iterations. Scheduled skill expansion must use
    /// model provenance, and maintenance sentinels must be read fresh by caller.
    pub(crate) fn begin_dynamic_iteration(&self, session: &str, prompt: &str) -> Result<String> {
        validate_text(prompt, "prompt", 16384)?;
        self.transact(session, |j| {
            clear_wakeup(j);
            j.wakeup_started = Some(Utc::now().timestamp());
            j.wakeup_prompt = Some(prompt.into());
            j.wakeup_iteration_active = true;
            let token = uuid::Uuid::new_v4().to_string();
            j.wakeup_iteration_token = Some(token.clone());
            Ok(token)
        })
    }
    /// Called after a completed dynamic iteration. An explicit schedule/stop
    /// clears active status, so this cannot override a model decision. Exactly
    /// one unscheduled iteration receives a fallback, never an endless chain.
    pub(crate) fn finish_wakeup_iteration(&self, session: &str, token: &str) -> Result<()> {
        let now = Utc::now().timestamp();
        self.transact(session, |j| {
            if !j.wakeup_iteration_active || j.wakeup_iteration_token.as_deref() != Some(token) {
                return Ok(());
            }
            j.wakeup_iteration_active = false;
            j.wakeup_iteration_token = None;
            let started = j.wakeup_started.unwrap_or(now);
            if j.wakeup_fallback_used || now >= started + WEEK {
                clear_wakeup(j);
                return Ok(());
            }
            if j.tasks.len() >= MAX_TASKS {
                return Err("session supports at most 50 scheduled tasks".into());
            }
            let prompt = j
                .wakeup_prompt
                .clone()
                .ok_or("dynamic loop prompt missing")?;
            j.wakeup_fallback_used = true;
            j.tasks.insert(
                "wakeup".into(),
                Task {
                    id: "wakeup".into(),
                    cron: None,
                    timezone: "local".into(),
                    prompt,
                    recurring: false,
                    created_at: started,
                    expires_at: started + WEEK,
                    next_fire_at: (now + 1200).min(started + WEEK),
                    reason: Some(
                        "Iteration ended without rescheduling; single 20-minute fallback".into(),
                    ),
                    jitter_seconds: 0,
                    noop_streak: j.noop_streak,
                },
            );
            Ok(())
        })
    }
    pub(crate) fn cancel_iteration(&self, session: &str, token: &str) -> Result<()> {
        self.transact(session, |j| {
            if j.wakeup_iteration_token.as_deref() == Some(token) {
                clear_wakeup(j);
            }
            Ok(())
        })
    }
    fn execute(
        &self,
        name: &str,
        input: Value,
        session: &str,
        turn: &str,
        call: &str,
    ) -> Result<Value> {
        let now = Utc::now().timestamp();
        let key = format!("{turn}:{call}");
        let fingerprint = hex::encode(Sha256::digest(
            serde_json::to_vec(&(name, &input)).map_err(|e| e.to_string())?,
        ));
        self.transact(session, |journal| {
            if let Some(receipt) = journal.receipts.get(&key) {
                if receipt.fingerprint != fingerprint { return Err("scheduler invocation changed after persistence".into()); }
                return Ok(receipt.output.clone());
            }
            if name != "CronList" && journal.receipts.len() >= MAX_RECEIPTS { return Err("session scheduler mutation receipt limit reached (4096)".into()); }
            let output = match name {
                "CronCreate" => {
                    let request: Create = serde_json::from_value(input).map_err(|e| e.to_string())?;
                    validate_text(&request.prompt, "prompt", 16384)?;
                    let cron = Cron::parse(&request.cron)?;
                    let nominal = cron.next(now, &request.timezone)?;
                    if journal.tasks.len() >= MAX_TASKS { return Err("session supports at most 50 scheduled tasks".into()); }
                    let id = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
                    if journal.tasks.contains_key(&id) { return Err("scheduler ID collision; retry with a new tool call".into()); }
                    let jitter = cron.jitter(&id, request.recurring, nominal, &request.timezone)?;
                    let next = (nominal + jitter).max(now + 1);
                    let task = Task { id: id.clone(), cron: Some(request.cron), timezone: request.timezone, prompt: request.prompt, recurring: request.recurring,
                        created_at: now, expires_at: if request.recurring {now + WEEK} else {next + WEEK}, next_fire_at: next, reason: None, jitter_seconds: jitter, noop_streak: 0 };
                    let output = json!({"id":id,"task":task,"scope":"session","runs_only_while_cli_open":true,"restored_on_resume":true,"jitter_seconds":jitter,"nominal_fire_at":nominal});
                    journal.tasks.insert(id, task); output
                }
                "CronList" => {
                    if input.as_object().is_none_or(|v| !v.is_empty()) { return Err("CronList requires an empty object".into()); }
                    json!({"tasks":journal.tasks.values().collect::<Vec<_>>(),"timezone_default":"local","runs_only_while_cli_open":true})
                },
                "CronDelete" => {
                    let request: Delete = serde_json::from_value(input).map_err(|e| e.to_string())?;
                    let removed = journal.tasks.remove(&request.id).is_some();
                    if request.id == "wakeup" { clear_wakeup(journal); }
                    json!({"id":request.id,"deleted":removed})
                }
                "ScheduleWakeup" => {
                    let request: Wakeup = serde_json::from_value(input).map_err(|e| e.to_string())?;
                    if request.stop {
                        let removed = journal.tasks.contains_key("wakeup"); clear_wakeup(journal); json!({"stopped":true,"cancelled_pending_wakeup":removed})
                    } else {
                        let delay = request.delay_seconds.ok_or("delaySeconds is required unless stop is true")?;
                        if !delay.is_finite() { return Err("delaySeconds must be finite".into()); }
                        let delay = delay.clamp(60.0,3600.0).ceil() as i64;
                        let prompt = request.prompt.ok_or("prompt is required unless stop is true")?;
                        let reason = request.reason.ok_or("reason is required unless stop is true")?;
                        let noop = request.noop.ok_or("noop is required unless stop is true")?;
                        validate_text(&prompt,"prompt",16384)?; validate_text(&reason,"reason",1024)?;
                        let previous = journal.tasks.get("wakeup");
                        if previous.is_none() && journal.tasks.len() >= MAX_TASKS { return Err("session supports at most 50 scheduled tasks".into()); }
                        let created = journal.wakeup_started.unwrap_or(now);
                        if now >= created + WEEK { return Err("dynamic loop expired after seven days; stop it before starting a new loop".into()); }
                        let streak = if noop { journal.noop_streak.saturating_add(1) } else {0};
                        journal.wakeup_started = Some(created); journal.noop_streak = streak;
                        journal.wakeup_prompt = Some(prompt.clone());
                        journal.wakeup_iteration_active = false;
                        journal.wakeup_fallback_used = false;
                        let task = Task { id:"wakeup".into(),cron:None,timezone:"local".into(),prompt,recurring:false,created_at:created,expires_at:created+WEEK,next_fire_at:(now+delay).min(created+WEEK),reason:Some(reason),jitter_seconds:0,noop_streak:streak };
                        let output = json!({"id":"wakeup","delaySeconds":delay,"task":task,"restored_on_resume":false});
                        journal.tasks.insert("wakeup".into(),task); output
                    }
                }
                _ => return Err("unknown scheduler tool".into()),
            };
            if name != "CronList" { journal.receipts.insert(key, Receipt { fingerprint, output: output.clone() }); }
            Ok(output)
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Create {
    cron: String,
    prompt: String,
    #[serde(default = "yes")]
    recurring: bool,
    #[serde(default = "local")]
    timezone: String,
    // Accepted for compatibility; native sessions always persist schedules.
    #[serde(default, rename = "durable")]
    _durable: bool,
}
const fn yes() -> bool {
    true
}
fn local() -> String {
    "local".into()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delete {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wakeup {
    #[serde(default)]
    stop: bool,
    #[serde(rename = "delaySeconds")]
    delay_seconds: Option<f64>,
    prompt: Option<String>,
    reason: Option<String>,
    noop: Option<bool>,
}
fn validate_text(text: &str, label: &str, max: usize) -> Result<()> {
    if text.trim().is_empty() || text.len() > max {
        return Err(format!("{label} must contain 1..{max} bytes"));
    }
    Ok(())
}
fn clear_wakeup(j: &mut Journal) {
    j.tasks.remove("wakeup");
    j.wakeup_started = None;
    j.wakeup_prompt = None;
    j.wakeup_iteration_active = false;
    j.wakeup_iteration_token = None;
    j.wakeup_fallback_used = false;
    j.noop_streak = 0;
}

struct Cron {
    fields: Vec<Vec<u32>>,
    dom_any: bool,
    dow_any: bool,
}
impl Cron {
    fn parse(text: &str) -> Result<Self> {
        if text.len() > 256 {
            return Err("cron exceeds 256 bytes".into());
        }
        let parts: Vec<_> = text.split_whitespace().collect();
        if parts.len() != 5 {
            return Err(
                "cron requires exactly five fields: minute hour day-of-month month day-of-week"
                    .into(),
            );
        }
        let mut fields = Vec::new();
        for (part, (low, high)) in parts
            .iter()
            .zip([(0, 59), (0, 23), (1, 31), (1, 12), (0, 7)])
        {
            let mut values = Vec::new();
            for item in part.split(',') {
                let mut step_parts = item.split('/');
                let range = step_parts.next().ok_or("empty cron field")?;
                let step = step_parts
                    .next()
                    .map(|n| number(n, 1, high + 1))
                    .transpose()?
                    .unwrap_or(1);
                if step_parts.next().is_some() {
                    return Err("invalid cron step".into());
                }
                let (start, end) = if range == "*" {
                    (low, high)
                } else if let Some((a, b)) = range.split_once('-') {
                    (number(a, low, high)?, number(b, low, high)?)
                } else {
                    let n = number(range, low, high)?;
                    (n, if item.contains('/') { high } else { n })
                };
                if start > end {
                    return Err("descending cron range".into());
                }
                values.extend((start..=end).step_by(step as usize));
            }
            values.sort_unstable();
            values.dedup();
            if values.is_empty() {
                return Err("empty cron field".into());
            }
            fields.push(values);
        }
        Ok(Self {
            fields,
            dom_any: parts[2].starts_with('*'),
            dow_any: parts[4].starts_with('*'),
        })
    }
    fn jitter(&self, id: &str, recurring: bool, nominal: i64, timezone: &str) -> Result<i64> {
        let hash = Sha256::digest(id.as_bytes());
        let seed = u64::from_be_bytes(hash[..8].try_into().map_err(|_| "invalid jitter digest")?);
        if recurring {
            // The smallest distance between daily wall-clock slots bounds all
            // sub-hour intervals, including irregular lists/ranges. A fixed
            // ID offset stays stable across calendar/DST changes.
            let slots: Vec<u32> = self.fields[1]
                .iter()
                .flat_map(|h| self.fields[0].iter().map(move |m| h * 60 + m))
                .collect();
            let mut interval = 1440;
            for pair in slots.windows(2) {
                interval = interval.min(pair[1] - pair[0]);
            }
            interval = interval.min(1440 + slots[0] - slots[slots.len() - 1]);
            let bound = 1800u64.min(u64::from(interval) * 30);
            Ok((seed % (bound + 1)) as i64)
        } else {
            let utc = Utc
                .timestamp_opt(nominal, 0)
                .single()
                .ok_or("schedule date out of range")?;
            let minute = if timezone == "local" {
                utc.with_timezone(&Local).minute()
            } else {
                utc.with_timezone(
                    &timezone
                        .parse::<chrono_tz::Tz>()
                        .map_err(|_| "invalid timezone")?,
                )
                .minute()
            };
            Ok(if minute == 0 || minute == 30 {
                -((seed % 91) as i64)
            } else {
                0
            })
        }
    }
    fn next(&self, after: i64, timezone: &str) -> Result<i64> {
        let zone = if timezone == "local" {
            None
        } else {
            Some(
                timezone
                    .parse::<chrono_tz::Tz>()
                    .map_err(|_| "timezone must be local or a valid IANA timezone")?,
            )
        };
        // Iterate calendar days, then matching wall-clock slots. Impossible
        // dates require at most 2928 cheap day checks, not millions of minute
        // conversions. Resolve DST gaps/overlaps with the selected zone.
        let utc = Utc
            .timestamp_opt(after, 0)
            .single()
            .ok_or("schedule date out of range")?;
        let mut date = if let Some(zone) = zone {
            utc.with_timezone(&zone).date_naive()
        } else {
            utc.with_timezone(&Local).date_naive()
        };
        for _ in 0..8 * 366 {
            let dow = date.weekday().num_days_from_sunday();
            let day = self.fields[2].contains(&date.day());
            let week = self.fields[4].contains(&dow) || (dow == 0 && self.fields[4].contains(&7));
            let matches_day = if self.dom_any || self.dow_any {
                day && week
            } else {
                day || week
            };
            if self.fields[3].contains(&date.month()) && matches_day {
                let mut best: Option<i64> = None;
                for hour in &self.fields[1] {
                    for minute in &self.fields[0] {
                        let slot = date
                            .and_hms_opt(*hour, *minute, 0)
                            .ok_or("invalid clock slot")?;
                        let resolved = if let Some(zone) = zone {
                            zone.from_local_datetime(&slot).map(|d| d.timestamp())
                        } else {
                            Local.from_local_datetime(&slot).map(|d| d.timestamp())
                        };
                        let candidates = match resolved {
                            LocalResult::Single(a) => [Some(a), None],
                            LocalResult::Ambiguous(a, b) => [Some(a), Some(b)],
                            LocalResult::None => [None, None],
                        };
                        for candidate in candidates
                            .into_iter()
                            .flatten()
                            .filter(|time| *time > after)
                        {
                            best = Some(best.map_or(candidate, |old| old.min(candidate)));
                        }
                    }
                }
                if let Some(best) = best {
                    return Ok(best);
                }
            }
            date = date.succ_opt().ok_or("schedule date out of range")?;
        }
        Err("cron has no occurrence within eight years".into())
    }
}
fn number(text: &str, low: u32, high: u32) -> Result<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err("cron supports numeric values, *, ranges, lists and steps only".into());
    }
    let n = text.parse::<u32>().map_err(|_| "invalid cron number")?;
    if !(low..=high).contains(&n) {
        return Err(format!("cron value must be in {low}..{high}"));
    }
    Ok(n)
}

pub(super) fn install(mut tools: ClaudeTools, scheduler: Arc<SessionScheduler>) -> ClaudeTools {
    for (name, description, schema) in [
        (
            "CronCreate",
            "Schedule a prompt in this session using five-field numeric cron (local time by default, optional IANA timezone). recurring defaults true; recurring jobs fire once finally and expire after seven days. At most 50 tasks. Runs only while this CLI is open and idle; unexpired cron tasks restore on resume, with no missed-run backlog. Task-ID deterministic jitter delays recurring tasks by up to 30 minutes (half the interval for shorter schedules) and advances :00/:30 one-shots by up to 90 seconds. One-shots are consumed before dispatch. durable is accepted but all native session tasks are persisted.",
            json!({"type":"object","properties":{"cron":{"type":"string"},"prompt":{"type":"string"},"recurring":{"type":"boolean"},"timezone":{"type":"string"},"durable":{"type":"boolean"}},"required":["cron","prompt"],"additionalProperties":false}),
        ),
        (
            "CronList",
            "List this session's cron tasks and pending dynamic wakeup, including IDs, prompts, timezone, next fire and expiry.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        (
            "CronDelete",
            "Cancel a scheduled task by ID. Returns deleted=false if it was already removed.",
            json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}),
        ),
        (
            "ScheduleWakeup",
            "Schedule the next dynamic-loop prompt after delaySeconds (clamped to 60..3600), replacing the pending wakeup. prompt, reason and noop are required unless stop=true. stop=true cancels it. Only runs in this open idle session; dynamic wakeups are not restored. An iteration without reschedule gets one 1200-second fallback; a second unscheduled iteration ends the loop. Dynamic delays have no jitter. Autonomous prompt sentinel: <<autonomous-loop-dynamic>>.",
            json!({"type":"object","properties":{"delaySeconds":{"type":"number"},"prompt":{"type":"string"},"reason":{"type":"string"},"noop":{"type":"boolean"},"stop":{"type":"boolean"}},"additionalProperties":false}),
        ),
    ] {
        let definition: ToolDefinition = serde_json::from_value(
            json!({"name":name,"description":description,"input_schema":schema}),
        )
        .expect("scheduler schema");
        let scheduler = Arc::clone(&scheduler);
        tools = tools.tool_with_context(definition, move |input, context| {
            let scheduler = Arc::clone(&scheduler);
            async move {
                scheduler
                    .execute(
                        name,
                        input,
                        &context.session_id,
                        &context.turn_id,
                        &context.call_id,
                    )
                    .map(|v| super::text_reply(v.to_string()))
            }
        });
    }
    tools
}
