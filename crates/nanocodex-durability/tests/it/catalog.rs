//! Black-box journeys through the family-neutral local session catalog.
#![cfg(feature = "sqlite")]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use eyre::{Result, eyre};
use nanocodex_agent::{
    ForkRequest, HarnessFamily, HarnessModel, Model, Nanocodex, OpenAi, Origin, PromptRequest,
    ResponseError, session::SessionId,
};
use nanocodex_durability::{
    BranchPoint, DurableAgentExt, SessionRecord, SessionStore, TranscriptItem, TurnStatus,
};

#[derive(Clone)]
struct ScriptedResponses {
    generations: Arc<AtomicUsize>,
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
    ($generations:expr) => {{
        let generations = Arc::clone($generations);
        OpenAi::builder("test-key")
            .service(move || ScriptedResponses {
                generations: Arc::clone(&generations),
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
/// branchable before a turn, and the branch resumes in a fresh process.
#[tokio::test]
async fn codex_session_lists_loads_branches_and_resumes_from_one_store() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let generations = Arc::new(AtomicUsize::new(0));
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
    let (agent, _events) = Nanocodex::builder(openai!(&generations)?)
        .model(Model::Luna)
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
    let (resumed, _events) = Nanocodex::builder(openai!(&generations)?)
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
