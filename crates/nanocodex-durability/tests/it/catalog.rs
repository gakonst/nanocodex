//! Black-box journeys through the family-neutral local session catalog.
#![cfg(feature = "sqlite")]

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use eyre::{Result, eyre};
use nanocodex_agent::{
    ForkRequest, HarnessFamily, HarnessModel, Model, Nanocodex, OpenAi, Origin, PromptRequest,
    ResponseError, ServiceTier, Thinking, session::SessionId,
};
use nanocodex_durability::{
    BranchPoint, DurableAgentExt, SessionRecord, SessionStore, TranscriptItem, TurnStatus,
};

#[derive(Clone)]
struct ScriptedResponses {
    generations: Arc<AtomicUsize>,
    /// Thinking and processing tier of every outbound generation request.
    policies: Arc<Mutex<Vec<(Thinking, ServiceTier)>>>,
}

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for ScriptedResponses {
    type Response = nanocodex_oai_api::tower::ResponsesServiceResponse;
    type Error = ResponseError;
    type Future = std::future::Ready<std::result::Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: nanocodex_oai_api::tower::ResponsesAttempt) -> Self::Future {
        use nanocodex_oai_api::{
            responses::{ContentItem, MessageRole, ResponseItem, WarmupResponse},
            tower::{
                GenerationOutput, ResponsePipelineStats, ResponsesAttemptKind, ResponsesOutput,
                ResponsesServiceResponse,
            },
        };
        let output = match request.kind() {
            ResponsesAttemptKind::Warmup => ResponsesOutput::Warmup(WarmupResponse {
                id: "warmup".to_owned(),
                usage: None,
            }),
            ResponsesAttemptKind::Generation => {
                self.policies
                    .lock()
                    .unwrap()
                    .push((request.thinking(), request.service_tier()));
                let reply = format!(
                    "reply {}",
                    self.generations.fetch_add(1, Ordering::SeqCst) + 1
                );
                ResponsesOutput::Generation(GenerationOutput {
                    id: format!("response-{reply}"),
                    reported_model: None,
                    status: "completed".to_owned(),
                    end_turn: Some(true),
                    final_message: Some(reply.clone()),
                    output_items: vec![ResponseItem::message(
                        MessageRole::Assistant,
                        [ContentItem::output_text(reply)],
                    )],
                    code_calls: Vec::new(),
                    usage: None,
                    time_to_first_event_ns: 0,
                    time_to_first_output_ns: None,
                    pipeline_stats: ResponsePipelineStats::default(),
                })
            }
            kind => panic!("unexpected attempt: {kind:?}"),
        };
        std::future::ready(Ok(ResponsesServiceResponse::new(output)))
    }
}

/// A Responses client whose generations are scripted and counted.
macro_rules! openai {
    ($generations:expr) => {
        openai!($generations, &Arc::new(Mutex::new(Vec::new())))
    };
    ($generations:expr, $policies:expr) => {{
        let generations = Arc::clone($generations);
        let policies = Arc::clone($policies);
        OpenAi::builder("test-key")
            .service(move || ScriptedResponses {
                generations: Arc::clone(&generations),
                policies: Arc::clone(&policies),
            })
            .build()
    }};
}

fn user_prompts(transcript: &[TranscriptItem]) -> Vec<&str> {
    transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::User(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// A Codex session recorded through the shared store is listable, readable,
/// branchable before a turn, and the branch resumes in a fresh process with the
/// session's own (non-default) thinking level and processing tier.
#[tokio::test]
async fn codex_session_lists_loads_branches_and_resumes_from_one_store() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let policies = Arc::new(Mutex::new(Vec::new()));
    assert_ne!(Model::Luna.default_thinking(), Thinking::High);
    let store = SessionStore::open(home.path())?;
    assert!(SessionStore::path(home.path()).is_file());
    assert!(
        store.list().await?.is_empty(),
        "a fresh store lists nothing"
    );

    let session_id = SessionId::default().to_string();
    let record = SessionRecord::root(
        session_id.clone(),
        HarnessModel::Codex(Model::Luna),
        Some(workspace.clone()),
    );
    let (agent, _events) = Nanocodex::builder(openai!(&generations, &policies)?)
        .model(Model::Luna)
        .thinking(Thinking::High)
        .service_tier(ServiceTier::Fast)
        .workspace(&workspace)
        .durability(store.session(record).await?)
        .await?
        .build()?;
    assert_eq!(
        agent.session_id(),
        session_id,
        "durable identity is the session identity"
    );
    assert_eq!(
        agent
            .persistence()
            .and_then(|persistence| persistence.durable_state_id),
        Some(session_id.clone())
    );
    agent
        .prompt(PromptRequest::new("first question").request_id("turn-1"))
        .await?
        .result()
        .await?;
    agent
        .prompt(PromptRequest::new("second question").request_id("turn-2"))
        .await?
        .result()
        .await?;

    // Listing while the owner is live must not fence it.
    let listed = store.list().await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].record.session_id, session_id);
    assert_eq!(listed[0].record.family(), HarnessFamily::Codex);
    assert_eq!(
        listed[0].record.workspace.as_deref(),
        Some(workspace.as_path())
    );
    assert_eq!(listed[0].preview.as_deref(), Some("first question"));
    agent
        .prompt(PromptRequest::new("third question").request_id("turn-3"))
        .await?
        .result()
        .await?;
    agent.shutdown().await?;

    // A separate process opens the same home read-only.
    let reader = SessionStore::open(home.path())?;
    let loaded = reader.load(&session_id).await?;
    assert_eq!(
        user_prompts(&loaded.transcript),
        ["first question", "second question", "third question"]
    );
    assert!(
        loaded
            .transcript
            .iter()
            .any(|item| matches!(item, TranscriptItem::Assistant(text) if text == "reply 3"))
    );
    let portable = loaded
        .session_checkpoint()?
        .ok_or_else(|| eyre!("a completed session has a checkpoint"))?;
    assert_eq!(portable.session_id(), session_id);
    assert_eq!(portable.family(), HarnessFamily::Codex);
    assert!(portable.has_conversation());
    // The stored session's actual settings, not the model's defaults.
    let live = *policies.lock().unwrap().first().expect("live requests");
    assert_eq!(live.0, Thinking::High);
    assert_ne!(live.1, ServiceTier::Standard);
    assert_eq!(portable.thinking(), Thinking::High);
    assert_eq!(
        portable.payload()["service_tier"],
        serde_json::to_value(live.1)?
    );
    let turns = reader.turns(&session_id).await?;
    assert_eq!(
        turns
            .iter()
            .map(|turn| turn.id.as_str())
            .collect::<Vec<_>>(),
        ["turn-1", "turn-2", "turn-3"]
    );
    assert!(
        turns
            .iter()
            .all(|turn| turn.status == TurnStatus::Completed)
    );

    // Branch before the second turn: only the first exchange survives.
    let before = reader
        .branch(&session_id, BranchPoint::Before("turn-2".into()), None)
        .await?;
    assert_eq!(before.record.lineage.origin, Origin::Branch);
    assert_eq!(
        before.record.lineage.parent_session_id.as_deref(),
        Some(session_id.as_str())
    );
    assert_eq!(before.record.lineage.root_session_id, session_id);
    let branched = reader.load(&before.record.session_id).await?;
    assert_eq!(user_prompts(&branched.transcript), ["first question"]);

    // Branching before the very first turn yields an empty, resumable session.
    let empty = reader
        .branch(&session_id, BranchPoint::Before("turn-1".into()), None)
        .await?;
    let empty_session = reader.load(&empty.record.session_id).await?;
    assert!(empty_session.transcript.is_empty());
    assert!(empty_session.checkpoint.is_none());

    // Branch through the second turn keeps it.
    let through = reader
        .branch(&session_id, BranchPoint::Through("turn-2".into()), None)
        .await?;
    assert_eq!(
        user_prompts(&reader.load(&through.record.session_id).await?.transcript),
        ["first question", "second question"]
    );
    assert!(matches!(
        reader
            .branch(&session_id, BranchPoint::Before("missing".into()), None)
            .await,
        Err(nanocodex_durability::Error::InvalidState(_))
    ));
    assert!(matches!(
        reader.load("missing-session").await,
        Err(nanocodex_durability::Error::SessionNotFound { .. })
    ));

    // The branch resumes as its own durable session and keeps its lineage.
    // The resuming host configures no thinking or tier: the stored ones apply.
    policies.lock().unwrap().clear();
    let (resumed, _events) = Nanocodex::builder(openai!(&generations, &policies)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .durability(reader.resume(&before.record.session_id).await?)
        .await?
        .build()?;
    assert_eq!(resumed.session_id(), before.record.session_id);
    resumed
        .prompt(PromptRequest::new("branch question").request_id("branch-1"))
        .await?
        .result()
        .await?;
    resumed.shutdown().await?;
    assert_eq!(
        *policies.lock().unwrap(),
        [live],
        "the resumed request uses the session's recorded thinking and tier"
    );
    let continued = reader.load(&before.record.session_id).await?;
    assert_eq!(
        user_prompts(&continued.transcript),
        ["first question", "branch question"]
    );
    assert_eq!(continued.summary.record.lineage.origin, Origin::Branch);
    // The source session was never modified by branching.
    assert_eq!(reader.turns(&session_id).await?.len(), 3);
    let ids = reader
        .list()
        .await?
        .into_iter()
        .map(|summary| summary.record.session_id)
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 3, "source and both branches are listed: {ids:?}");
    Ok(())
}

/// A fork of a durable root persists as its own resumable session with lineage.
#[tokio::test]
async fn durable_fork_is_its_own_resumable_session() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    root.prompt(PromptRequest::new("root question").request_id("root-1"))
        .await?
        .result()
        .await?;
    let (fork, _fork_events) = root.fork(ForkRequest::latest()).await?;
    let fork_id = fork.session_id().to_owned();
    assert_ne!(fork_id, root_id);
    assert_eq!(fork.session().lineage.origin, Origin::Fork);
    assert_eq!(
        fork.persistence()
            .and_then(|persistence| persistence.durable_state_id),
        Some(fork_id.clone()),
        "a durable root's fork persists to its own state"
    );
    fork.prompt(PromptRequest::new("fork question").request_id("fork-1"))
        .await?
        .result()
        .await?;
    fork.shutdown().await?;
    root.shutdown().await?;

    let stored = store.load(&fork_id).await?;
    assert_eq!(stored.summary.record.lineage.origin, Origin::Fork);
    assert_eq!(
        stored.summary.record.lineage.parent_session_id.as_deref(),
        Some(root_id.as_str())
    );
    assert_eq!(
        user_prompts(&stored.transcript),
        ["root question", "fork question"]
    );
    let (resumed, _events) = Nanocodex::builder(openai!(&generations)?)
        .workspace(&workspace)
        .durability(store.resume(&fork_id).await?)
        .await?
        .build()?;
    assert_eq!(resumed.session_id(), fork_id);
    resumed
        .prompt(PromptRequest::new("resumed fork").request_id("fork-2"))
        .await?
        .result()
        .await
        .map_err(|error| eyre!("resumed fork prompt failed: {error}"))?;
    resumed.shutdown().await?;
    assert_eq!(store.turns(&fork_id).await?.len(), 2);
    Ok(())
}

/// A subagent of a durable root is its own listed, readable and resumable
/// session with its own Codex rollout, exactly like a fork.
#[tokio::test]
async fn durable_subagents_are_their_own_resumable_sessions() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let rollout = nanocodex_agent::rollout::RolloutConfig::new(home.path().join("codex"));
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .rollout(rollout.clone())
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    root.prompt(PromptRequest::new("root task").request_id("root-1"))
        .await?
        .result()
        .await?;
    let (child, _child_events) = root.spawn().await?;
    let (grandchild, _grandchild_events) = child.spawn().await?;
    let child_id = child.session_id().to_owned();
    let grandchild_id = grandchild.session_id().to_owned();
    assert_ne!(child_id, root_id);
    assert_ne!(grandchild_id, child_id);
    let tree = [(&child_id, &root_id), (&grandchild_id, &child_id)];
    for (agent, (id, parent)) in [&child, &grandchild].into_iter().zip(tree) {
        assert_eq!(agent.session().lineage.origin, Origin::Subagent);
        assert_eq!(
            agent.session().lineage.parent_session_id.as_deref(),
            Some(parent.as_str())
        );
        let persistence = agent.persistence().expect("a durable root's subagent persists");
        assert_eq!(persistence.durable_state_id.as_deref(), Some(id.as_str()));
        assert!(persistence.rollout.is_some(), "every subagent mirrors a Codex rollout");
        agent
            .prompt(PromptRequest::new("subagent task").request_id("task-1"))
            .await?
            .result()
            .await?;
    }
    grandchild.shutdown().await?;
    child.shutdown().await?;
    root.shutdown().await?;

    let listed = store.list().await?;
    for (id, parent) in tree {
        let summary = listed
            .iter()
            .find(|summary| summary.record.session_id == *id)
            .ok_or_else(|| eyre!("subagent {id} is not listed"))?;
        assert_eq!(summary.record.lineage.origin, Origin::Subagent);
        assert_eq!(
            summary.record.lineage.parent_session_id.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(summary.record.lineage.root_session_id, root_id);
        let stored = store.load(id).await?;
        assert_eq!(user_prompts(&stored.transcript), ["subagent task"]);
    }
    let mirrored = |id: &str| -> Result<usize> {
        Ok(rollout
            .list_sessions()?
            .iter()
            .filter(|session| session.thread_id() == id)
            .count())
    };
    for id in [&root_id, &child_id, &grandchild_id] {
        assert_eq!(mirrored(id)?, 1, "{id} has exactly one Codex rollout");
    }

    let (resumed, _events) = Nanocodex::builder(openai!(&generations)?)
        .workspace(&workspace)
        .rollout(rollout.clone())
        .durability(store.resume(&child_id).await?)
        .await?
        .build()?;
    assert_eq!(resumed.session_id(), child_id);
    resumed
        .prompt(PromptRequest::new("resumed subagent").request_id("task-2"))
        .await?
        .result()
        .await
        .map_err(|error| eyre!("resumed subagent prompt failed: {error}"))?;
    resumed.shutdown().await?;
    assert_eq!(store.turns(&child_id).await?.len(), 2);
    assert_eq!(
        user_prompts(&store.load(&child_id).await?.transcript),
        ["subagent task", "resumed subagent"]
    );
    Ok(())
}

/// A durable branch reopened for the first time creates its Codex rollout
/// with the branch's provenance (role, parent and root), not a root resume.
#[tokio::test]
async fn reopened_branches_mirror_their_provenance() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let rollout = nanocodex_agent::rollout::RolloutConfig::new(home.path().join("codex"));
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .rollout(rollout.clone())
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    root.prompt(PromptRequest::new("root task").request_id("root-1"))
        .await?
        .result()
        .await?;
    root.shutdown().await?;

    let branch = store.branch(&root_id, BranchPoint::Latest, None).await?;
    let branch_id = branch.record.session_id.clone();
    let (reopened, _events) = Nanocodex::builder(openai!(&generations)?)
        .workspace(&workspace)
        .rollout(rollout.clone())
        .durability(store.resume(&branch_id).await?)
        .await?
        .build()?;
    assert_eq!(reopened.session_id(), branch_id);
    assert_eq!(reopened.session().lineage.origin, Origin::Branch);
    reopened
        .prompt(PromptRequest::new("branch task").request_id("branch-1"))
        .await?
        .result()
        .await?;
    reopened.shutdown().await?;

    let mirrored = rollout
        .list_sessions()?
        .into_iter()
        .find(|session| session.thread_id() == branch_id)
        .ok_or_else(|| eyre!("branch {branch_id} has no Codex rollout"))?;
    assert_eq!(mirrored.origin(), Origin::Fork, "recorded as a branch role");
    assert_eq!(mirrored.parent_session_id(), Some(root_id.as_str()));
    assert_eq!(mirrored.root_session_id(), root_id);
    Ok(())
}


type Handles = Arc<Mutex<std::collections::HashMap<String, nanocodex_agent::AgentHandle>>>;

/// Codex JSONL files recorded for one thread anywhere under a Codex home.
fn rollout_files(codex_home: &std::path::Path, id: &str) -> Result<Vec<std::path::PathBuf>> {
    let mut found = Vec::new();
    let mut directories = vec![codex_home.join("sessions")];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                directories.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("{id}.jsonl")))
            {
                found.push(path);
            }
        }
    }
    Ok(found)
}

/// Captures every driver's weak capability, keyed by session, so a test can
/// drive the parent-side lifecycle (atomic batches, restoring an evicted child).
fn capturing_tools(
    handles: &Handles,
) -> impl Fn(nanocodex_agent::AgentHandle) -> std::result::Result<nanocodex_agent::Tools, nanocodex_oai_tools::ToolsBuildError>
+ Send
+ Sync
+ 'static {
    let handles = Arc::clone(handles);
    move |handle| {
        handles
            .lock()
            .unwrap()
            .insert(handle.session_id().to_owned(), handle);
        nanocodex_agent::Tools::builder().without_defaults().build()
    }
}

/// Every child of a durable root (fork, side conversation, subagent and nested
/// subagent) is listed, loadable and mirrored by exactly one Codex rollout as
/// soon as it is created, before any child prompt, and after the whole tree
/// shuts down it resumes in a fresh store with its identity and lineage
/// without ever having been prompted.
#[tokio::test]
async fn durable_children_are_listed_and_resumable_before_their_first_prompt() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let rollout = nanocodex_agent::rollout::RolloutConfig::new(home.path().join("codex"));
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .rollout(rollout.clone())
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    root.prompt(PromptRequest::new("root task").request_id("root-1"))
        .await?
        .result()
        .await?;
    let (fork, _fork_events) = root.fork(ForkRequest::latest()).await?;
    let (side, _side_events) = root
        .fork(ForkRequest::latest().side_conversation())
        .await?;
    let (child, _child_events) = root.spawn().await?;
    let (grandchild, _grandchild_events) = child.spawn().await?;
    let child_id = child.session_id().to_owned();
    let expected = [
        (fork.session_id().to_owned(), Origin::Fork, root_id.clone(), vec!["root task"]),
        (
            side.session_id().to_owned(),
            Origin::SideConversation,
            root_id.clone(),
            vec!["root task"],
        ),
        (child_id.clone(), Origin::Subagent, root_id.clone(), vec![]),
        (
            grandchild.session_id().to_owned(),
            Origin::Subagent,
            child_id.clone(),
            vec![],
        ),
    ];
    let mirrors = |id: &str| -> Result<Vec<nanocodex_agent::rollout::RolloutSessionInfo>> {
        Ok(rollout
            .list_sessions()?
            .into_iter()
            .filter(|session| session.thread_id() == id)
            .collect())
    };

    // Before any child prompt: listed, loadable with a resumable checkpoint
    // and exactly one Codex rollout carrying the same provenance.
    let listed = store.list().await?;
    for (id, origin, parent, prompts) in &expected {
        let summary = listed
            .iter()
            .find(|summary| summary.record.session_id == *id)
            .ok_or_else(|| eyre!("{origin:?} child {id} is not listed before its first prompt"))?;
        assert_eq!(summary.record.family(), HarnessFamily::Codex);
        assert_eq!(summary.record.lineage.origin, *origin);
        assert_eq!(
            summary.record.lineage.parent_session_id.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(summary.record.lineage.root_session_id, root_id);
        let stored = store.load(id).await?;
        assert_eq!(user_prompts(&stored.transcript), *prompts, "{origin:?} transcript");
        assert!(stored.turns.is_empty(), "{origin:?} child has no turn yet");
        // A fork starts from its inherited boundary; a fresh subagent has no
        // conversation yet and resumes from its recorded identity alone.
        assert_eq!(
            stored.session_checkpoint()?.is_some(),
            !prompts.is_empty(),
            "{origin:?} child's initial checkpoint"
        );
        // The child's Codex JSONL exists from creation with its provenance;
        // Codex lists it once it holds a turn.
        let files = rollout_files(&home.path().join("codex"), id)?;
        assert_eq!(files.len(), 1, "{origin:?} child has exactly one Codex rollout file");
        let meta: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&files[0])?
                .lines()
                .next()
                .unwrap_or_default(),
        )?;
        assert_eq!(meta["type"], "session_meta");
        assert_eq!(meta["payload"]["id"], id.as_str());
        assert_eq!(meta["payload"]["parent_thread_id"], parent.as_str());
        assert_eq!(meta["payload"]["root_session_id"], root_id.as_str());
    }
    assert_eq!(
        generations.load(Ordering::SeqCst),
        1,
        "creating children sends no model request"
    );

    // Shut the whole tree down without prompting any child, then reopen every
    // child from a fresh store as a fresh process would.
    for agent in [&grandchild, &child, &side, &fork, &root] {
        agent.shutdown().await?;
    }
    drop(store);
    let store = SessionStore::open(home.path())?;
    for (id, origin, parent, prompts) in &expected {
        let (resumed, _events) = Nanocodex::builder(openai!(&generations)?)
            .workspace(&workspace)
            .rollout(rollout.clone())
            .durability(store.resume(id).await?)
            .await?
            .build()?;
        assert_eq!(resumed.session_id(), id.as_str());
        assert_eq!(resumed.session().lineage.origin, *origin);
        assert_eq!(
            resumed.session().lineage.parent_session_id.as_deref(),
            Some(parent.as_str())
        );
        assert_eq!(resumed.session().lineage.root_session_id, root_id);
        resumed
            .prompt(PromptRequest::new("first child prompt").request_id("child-1"))
            .await?
            .result()
            .await
            .map_err(|error| eyre!("{origin:?} child failed its first prompt: {error}"))?;
        resumed.shutdown().await?;
        let mut prompts = prompts.clone();
        prompts.push("first child prompt");
        assert_eq!(user_prompts(&store.load(id).await?.transcript), prompts);
        assert_eq!(store.turns(id).await?.len(), 1);
        let mirrored = mirrors(id)?;
        assert_eq!(mirrored.len(), 1, "{origin:?} child is mirrored exactly once");
        assert_eq!(mirrored[0].origin(), *origin);
        assert_eq!(mirrored[0].parent_session_id(), Some(parent.as_str()));
        assert_eq!(mirrored[0].root_session_id(), root_id);
        assert_eq!(rollout_files(&home.path().join("codex"), id)?.len(), 1);
    }
    Ok(())
}

/// Restoring an evicted subagent from a checkpoint taken before its first turn
/// keeps the history its durable state already holds.
#[tokio::test]
async fn restored_subagent_keeps_its_durable_history() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let handles: Handles = Arc::default();
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .tools_factory(capturing_tools(&handles))
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    let (child, _child_events) = root.spawn().await?;
    let child_id = child.session_id().to_owned();
    let stale = child.checkpoint().await?;
    child
        .prompt(PromptRequest::new("child task").request_id("child-1"))
        .await?
        .result()
        .await?;
    child.shutdown().await?;
    let owner = handles.lock().unwrap()[&root_id].clone();
    let (restored, _restored_events) = owner.restore_runtime(stale, None).await?;
    assert_eq!(restored.session_id(), child_id);
    assert_eq!(restored.session().lineage.origin, Origin::Subagent);
    assert_eq!(
        restored.session().lineage.parent_session_id.as_deref(),
        Some(root_id.as_str())
    );
    let stored = store.load(&child_id).await?;
    assert_eq!(user_prompts(&stored.transcript), ["child task"]);
    assert_eq!(stored.turns.len(), 1);
    restored.shutdown().await?;
    root.shutdown().await?;
    Ok(())
}

/// Real SQLite storage that rejects the nth new child state written after it
/// is armed, as a full disk or lost connection would.
struct FaultySqlite {
    inner: nanocodex_durability::SqliteStore,
    fail_nth: Arc<AtomicUsize>,
    seen: Arc<AtomicUsize>,
}

impl nanocodex_durability::StateStore for FaultySqlite {
    fn read_record<'a>(
        &'a mut self,
        state_id: &'a str,
        key: &'a str,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        std::result::Result<Option<String>, nanocodex_durability::StoreError>,
    > {
        self.inner.read_record(state_id, key)
    }

    fn read_records<'a>(
        &'a mut self,
        state_id: &'a str,
        keys: &'a [String],
    ) -> nanocodex_durability::StoreFuture<
        'a,
        std::result::Result<Vec<Option<String>>, nanocodex_durability::StoreError>,
    > {
        self.inner.read_records(state_id, keys)
    }

    fn peek<'a>(
        &'a mut self,
        state_id: &'a str,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        std::result::Result<nanocodex_durability::StoredState, nanocodex_durability::StoreError>,
    > {
        self.inner.peek(state_id)
    }

    fn list_states<'a>(
        &'a mut self,
        limit: usize,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        std::result::Result<Vec<String>, nanocodex_durability::StoreError>,
    > {
        self.inner.list_states(limit)
    }

    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: nanocodex_durability::OwnerId,
    ) -> nanocodex_durability::StoreFuture<
        'a,
        std::result::Result<nanocodex_durability::OwnedState, nanocodex_durability::StoreError>,
    > {
        self.inner.acquire(state_id, owner_id)
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a nanocodex_durability::OwnerToken,
        expected_revision: u64,
        payload: &'a str,
        records: &'a [nanocodex_durability::StoreRecord],
    ) -> nanocodex_durability::StoreFuture<
        'a,
        std::result::Result<u64, nanocodex_durability::StoreError>,
    > {
        // A new child's state is first written when it is described.
        let fail_nth = self.fail_nth.load(Ordering::SeqCst);
        if fail_nth != 0
            && expected_revision == 0
            && self.seen.fetch_add(1, Ordering::SeqCst) + 1 == fail_nth
        {
            return Box::pin(async {
                Err(nanocodex_durability::StoreError::NotCommitted(
                    "injected new-child write failure".to_owned(),
                ))
            });
        }
        self.inner
            .replace(state_id, owner, expected_revision, payload, records)
    }
}

/// An atomic subagent batch whose second child cannot persist leaves no child
/// listed and keeps the parent's history; a later batch persists every child.
#[tokio::test]
async fn failed_atomic_batch_leaves_no_listed_children() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let fail_nth = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::new(FaultySqlite {
        inner: nanocodex_durability::SqliteStore::open(SessionStore::path(home.path()))?,
        fail_nth: Arc::clone(&fail_nth),
        seen: Arc::new(AtomicUsize::new(0)),
    })?;
    let handles: Handles = Arc::default();
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
        .workspace(&workspace)
        .tools_factory(capturing_tools(&handles))
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    root.prompt(PromptRequest::new("root task").request_id("root-1"))
        .await?
        .result()
        .await?;
    let owner = handles.lock().unwrap()[&root_id].clone();
    let children_of_root = |listed: &[nanocodex_durability::SessionSummary]| {
        listed
            .iter()
            .filter(|summary| {
                summary.record.lineage.parent_session_id.as_deref() == Some(root_id.as_str())
            })
            .count()
    };

    fail_nth.store(2, Ordering::SeqCst);
    let failed = owner.spawn_many(3).await;
    assert!(failed.is_err(), "a batch with an unpersisted child fails");
    fail_nth.store(0, Ordering::SeqCst);
    let listed = store.list().await?;
    assert_eq!(children_of_root(&listed), 0, "a failed batch leaves no listed child");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        user_prompts(&store.load(&root_id).await?.transcript),
        ["root task"],
        "the parent's history survives the failed batch"
    );

    let children = owner.spawn_many(2).await?;
    let listed = store.list().await?;
    assert_eq!(children_of_root(&listed), 2, "a complete batch lists every child");
    for (child, _events) in &children {
        let stored = store.load(child.session_id()).await?;
        assert_eq!(stored.summary.record.lineage.origin, Origin::Subagent);
        assert!(stored.turns.is_empty());
        child.shutdown().await?;
    }
    root.shutdown().await?;
    Ok(())
}



/// A subagent created with its own model, reasoning effort and processing
/// tier keeps them from creation: its catalog record names its model before
/// any prompt, and after a restart before its first prompt its first model
/// request uses the recorded effort and tier on that model.
#[tokio::test]
async fn new_subagent_keeps_its_settings_across_a_restart_before_its_first_prompt() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
    let policies = Arc::new(Mutex::new(Vec::new()));
    let store = SessionStore::open(home.path())?;
    let root_id = SessionId::default().to_string();
    let (root, _events) = Nanocodex::builder(openai!(&generations, &policies)?)
        .model(Model::Luna)
        .service_tier(ServiceTier::Fast)
        .workspace(&workspace)
        .durability(
            store
                .session(SessionRecord::root(
                    root_id.clone(),
                    HarnessModel::Codex(Model::Luna),
                    Some(workspace.clone()),
                ))
                .await?,
        )
        .await?
        .build()?;
    let (child, _child_events) = root
        .spawn_with(
            nanocodex_agent::SpawnOptions::new()
                .model(Model::Sol)
                .thinking(Thinking::High),
        )
        .await?;
    let child_id = child.session_id().to_owned();
    let stored = store.load(&child_id).await?;
    assert_eq!(stored.summary.record.model, HarnessModel::Codex(Model::Sol));
    assert_eq!(stored.summary.record.lineage.origin, Origin::Subagent);
    assert!(stored.turns.is_empty());
    assert_eq!(generations.load(Ordering::SeqCst), 0, "no model request yet");
    child.shutdown().await?;
    root.shutdown().await?;

    drop(store);
    let store = SessionStore::open(home.path())?;
    // A plain builder: no model, effort or tier configuration.
    let (resumed, _events) = Nanocodex::builder(openai!(&generations, &policies)?)
        .workspace(&workspace)
        .durability(store.resume(&child_id).await?)
        .await?
        .build()?;
    assert_eq!(resumed.session_id(), child_id);
    resumed
        .prompt(PromptRequest::new("first child prompt").request_id("child-1"))
        .await?
        .result()
        .await?;
    assert_eq!(
        policies.lock().unwrap().last().copied(),
        Some((Thinking::High, ServiceTier::Fast)),
        "the first request uses the recorded effort and tier"
    );
    let checkpoint = resumed.checkpoint().await?;
    assert_eq!(checkpoint.model(), HarnessModel::Codex(Model::Sol));
    assert_eq!(checkpoint.thinking(), Thinking::High);
    resumed.shutdown().await?;
    assert_eq!(store.load(&child_id).await?.summary.record.model, HarnessModel::Codex(Model::Sol));
    Ok(())
}

