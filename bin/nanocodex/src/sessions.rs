//! One catalog of resumable sessions for every harness family.
//!
//! The durable session store is the single source of truth for local sessions
//! of both families. Codex-compatible JSONL rollouts are written as a mirror;
//! rollouts without durable state (older Codex threads or imported files) stay
//! listable and resumable through this same catalog.
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use eyre::{Result, WrapErr, eyre};
use nanocodex::{
    HarnessFamily, HarnessModel,
    agent::{
        rollout::RolloutConfig,
        session::{SessionCheckpoint, TranscriptItem},
    },
};
use nanocodex_durability::{BranchPoint, DurableSession, SessionRecord, SessionStore, StoredTurn};

/// Store written by Claude sessions before both families shared one store.
fn legacy_store_path(home: &Path) -> PathBuf {
    home.join("claude/sessions.sqlite")
}

/// Durable stores searched for sessions, newest layout first.
fn store_paths(home: &Path) -> [PathBuf; 2] {
    [SessionStore::path(home), legacy_store_path(home)]
}

fn open_existing(path: &Path) -> Result<Option<SessionStore>> {
    if !path.is_file() {
        return Ok(None);
    }
    SessionStore::open_path(path)
        .map(Some)
        .wrap_err_with(|| format!("failed to open the session store {}", path.display()))
}

/// One listed session of either family.
#[derive(Clone, Debug)]
pub(crate) struct SessionSummary {
    id: String,
    family: HarnessFamily,
    model: Option<HarnessModel>,
    workspace: Option<PathBuf>,
    preview: Option<String>,
    modified_at: SystemTime,
    archived: bool,
    store: Option<PathBuf>,
}

impl SessionSummary {
    fn from_record(record: SessionRecord, preview: Option<String>, store: &Path) -> Self {
        Self {
            family: record.family(),
            model: Some(record.model),
            workspace: record.workspace,
            preview: preview.or(record.title),
            modified_at: UNIX_EPOCH + Duration::from_millis(record.updated_at_ms),
            archived: false,
            store: Some(store.to_path_buf()),
            id: record.session_id,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) const fn family(&self) -> HarnessFamily {
        self.family
    }

    pub(crate) const fn model(&self) -> Option<HarnessModel> {
        self.model
    }

    pub(crate) fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref().and_then(Path::to_str)
    }

    pub(crate) fn preview(&self) -> Option<&str> {
        self.preview.as_deref()
    }

    pub(crate) const fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    /// Whether a rollout-only session was archived by Codex.
    pub(crate) const fn is_archived(&self) -> bool {
        self.archived
    }
}

/// A stored session selected for continuation in its recording family.
#[derive(Clone, Debug)]
pub(crate) struct ResumedSession {
    summary: SessionSummary,
    model: HarnessModel,
    transcript: Vec<TranscriptItem>,
    /// Boundary of a rollout-only Codex thread that has no durable state yet.
    rollout: Option<RolloutBoundary>,
}

#[derive(Clone, Debug)]
struct RolloutBoundary {
    checkpoint: Box<SessionCheckpoint>,
    /// The thread's rollout, continued in place by the mirror.
    mirror: RolloutConfig,
}

impl ResumedSession {
    pub(crate) fn id(&self) -> &str {
        self.summary.id()
    }

    pub(crate) const fn family(&self) -> HarnessFamily {
        self.summary.family
    }

    /// Model pinned by the stored session.
    pub(crate) const fn model(&self) -> HarnessModel {
        self.model
    }

    pub(crate) fn workspace(&self) -> Option<&Path> {
        self.summary.workspace.as_deref()
    }

    /// Visible conversation restored into the interactive transcript.
    pub(crate) fn transcript(&self) -> &[TranscriptItem] {
        &self.transcript
    }

    /// Codex boundary to restore when the durable store has no state for it.
    pub(crate) fn fallback(&self) -> Option<&SessionCheckpoint> {
        self.rollout
            .as_ref()
            .map(|rollout| rollout.checkpoint.as_ref())
    }

    /// Rollout mirror that continues this session's existing rollout file.
    fn mirror(&self, home: &Path) -> RolloutConfig {
        if let Some(rollout) = &self.rollout {
            return rollout.mirror.clone();
        }
        RolloutConfig::new(home)
            .load_session(self.id())
            .map_or_else(|_| RolloutConfig::new(home), |thread| thread.into_parts().2)
    }

    /// Durable store holding this session, when it is not the shared default.
    pub(crate) fn store(&self) -> Option<&Path> {
        self.summary.store.as_deref()
    }
}

/// Lists resumable sessions of every family, most recently updated first.
pub(crate) async fn list(home: &Path) -> Result<Vec<SessionSummary>> {
    let mut sessions = Vec::new();
    let mut seen = HashSet::new();
    for path in store_paths(home) {
        let Some(store) = open_existing(&path)? else {
            continue;
        };
        for listed in store
            .list()
            .await
            .wrap_err("failed to list stored sessions")?
        {
            if seen.insert(listed.record.session_id.clone()) {
                sessions.push(SessionSummary::from_record(
                    listed.record,
                    listed.preview,
                    &path,
                ));
            }
        }
    }
    let rollouts = match RolloutConfig::new(home).list_sessions() {
        Ok(rollouts) => rollouts,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(error)
                .wrap_err_with(|| format!("failed to discover rollouts under {}", home.display()));
        }
    };
    for info in rollouts {
        // Mirrors of durable sessions are listed once, from their store.
        if !seen.insert(info.thread_id().to_owned()) {
            continue;
        }
        sessions.push(SessionSummary {
            id: info.thread_id().to_owned(),
            family: info.harness_family().unwrap_or(HarnessFamily::Codex),
            model: info.harness_model(),
            workspace: info.workspace().map(PathBuf::from),
            preview: info.preview().map(str::to_owned),
            modified_at: info.modified_at(),
            archived: info.is_archived(),
            store: None,
        });
    }
    sessions.sort_by_key(|session| std::cmp::Reverse(session.modified_at));
    Ok(sessions)
}

/// Loads one session of either family for resume.
pub(crate) async fn load(home: &Path, id: &str) -> Result<ResumedSession> {
    if id.len() > 256 {
        return Err(eyre!("session ID is too long"));
    }
    for path in store_paths(home) {
        let Some(store) = open_existing(&path)? else {
            continue;
        };
        if store
            .summary(id)
            .await
            .wrap_err_with(|| format!("failed to inspect session {id}"))?
            .is_none()
        {
            continue;
        }
        let stored = store
            .load(id)
            .await
            .wrap_err_with(|| format!("failed to load session {id}"))?;
        return Ok(ResumedSession {
            model: stored.summary.record.model,
            summary: SessionSummary::from_record(
                stored.summary.record,
                stored.summary.preview,
                &path,
            ),
            transcript: stored.transcript,
            rollout: None,
        });
    }
    let session = RolloutConfig::new(home)
        .load_session(id)
        .wrap_err_with(|| {
            format!(
                "unknown session {id}: nothing saved under {}",
                home.display()
            )
        })?;
    let checkpoint = session
        .checkpoint()
        .wrap_err_with(|| format!("failed to read the boundary of rollout {id}"))?;
    let transcript = session.transcript().to_vec();
    let info = SessionSummary {
        id: session.thread_id().to_owned(),
        family: HarnessFamily::Codex,
        model: Some(checkpoint.model()),
        workspace: Some(PathBuf::from(session.workspace())),
        preview: transcript.iter().find_map(|item| match item {
            TranscriptItem::User(text) => Some(text.clone()),
            _ => None,
        }),
        modified_at: SystemTime::now(),
        archived: false,
        store: None,
    };
    let (_, _, mirror) = session.into_parts();
    Ok(ResumedSession {
        model: checkpoint.model(),
        transcript,
        rollout: Some(RolloutBoundary {
            checkpoint: Box::new(checkpoint),
            mirror,
        }),
        summary: info,
    })
}

/// Where and how one root session persists, identical for both families.
pub(crate) struct Persistence {
    store: PathBuf,
    state_id: String,
    mirror: Option<RolloutConfig>,
}

impl Persistence {
    /// The shared local store under a Codex home.
    pub(crate) fn shared(home: &Path, session_id: &str, mirror: Option<RolloutConfig>) -> Self {
        Self::new(SessionStore::path(home), session_id.to_owned(), mirror)
    }

    /// An explicit store and state, such as a durability test database.
    pub(crate) const fn new(
        store: PathBuf,
        state_id: String,
        mirror: Option<RolloutConfig>,
    ) -> Self {
        Self {
            store,
            state_id,
            mirror,
        }
    }

    /// Continues a resumed session in the store that holds it, appending to
    /// its existing rollout mirror when recording rollouts.
    pub(crate) fn resumed(session: &ResumedSession, home: &Path, rollouts: bool) -> Self {
        let store = session
            .store()
            .map_or_else(|| SessionStore::path(home), Path::to_path_buf);
        let mirror = rollouts.then(|| session.mirror(home));
        Self::new(store, session.id().to_owned(), mirror)
    }

    /// Codex-compatible JSONL mirror, when recording rollouts.
    pub(crate) fn mirror(&self) -> Option<RolloutConfig> {
        self.mirror.clone()
    }

    /// Opens or creates this session's durable state and records its metadata.
    pub(crate) async fn open(
        &self,
        model: HarnessModel,
        workspace: &Path,
    ) -> Result<DurableSession> {
        if let Some(parent) = self.store.parent() {
            std::fs::create_dir_all(parent).wrap_err_with(|| {
                format!(
                    "failed to create the session store directory {}",
                    parent.display()
                )
            })?;
        }
        let store = SessionStore::open_path(&self.store).wrap_err_with(|| {
            format!("failed to open the session store {}", self.store.display())
        })?;
        let existing = store
            .summary(&self.state_id)
            .await
            .wrap_err("failed to inspect the durable session")?;
        drop(store);
        // The owning process writes through its own connection so shutdown
        // cancellations commit before the runtime stops.
        let state = DurableSession::open(
            nanocodex_durability::SqliteStore::open(&self.store)?,
            self.state_id.clone(),
        )
        .await
        .wrap_err("failed to open the durable session")?;
        if existing.is_some() {
            return Ok(state);
        }
        state
            .describe(SessionRecord::root(
                self.state_id.clone(),
                model,
                Some(workspace.to_path_buf()),
            ))
            .await
            .wrap_err("failed to record the session metadata")?;
        Ok(state)
    }
}

/// Retained user turns of a stored session, for branch previews.
pub(crate) async fn turns(home: &Path, id: &str) -> Result<(PathBuf, Vec<StoredTurn>)> {
    for path in store_paths(home) {
        let Some(store) = open_existing(&path)? else {
            continue;
        };
        if store.summary(id).await?.is_some() {
            let turns = store.turns(id).await?;
            return Ok((path, turns));
        }
    }
    Err(eyre!(
        "session {id} has no durable history under {}",
        home.display()
    ))
}

/// Publishes a new session from a stored boundary of either family.
pub(crate) async fn branch(
    store: &Path,
    id: &str,
    at: BranchPoint,
    workspace: Option<PathBuf>,
) -> Result<SessionSummary> {
    let sessions = SessionStore::open_path(store)?;
    let family = sessions
        .summary(id)
        .await?
        .ok_or_else(|| eyre!("unknown session {id}"))?
        .record
        .family();
    let branched = sessions
        .branch_with(id, at, workspace, move |selected, latest| {
            match (family, latest) {
                // Only the provider-specific truncation differs by family.
                (HarnessFamily::Claude, Some(latest)) => {
                    nanocodex::claude::rewind_checkpoint(id, selected, latest.clone())
                        .map(Some)
                        .map_err(|error| {
                            nanocodex_durability::Error::InvalidState(error.to_string())
                        })
                }
                _ => Ok(selected),
            }
        })
        .await?;
    Ok(SessionSummary::from_record(
        branched.record,
        branched.preview,
        store,
    ))
}
