//! Branch boundaries recorded at creation through the public session catalog.
#![cfg(feature = "sqlite")]

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use eyre::Result;
use nanocodex_agent::{
    ForkRequest, HarnessModel, Model, Nanocodex, OpenAi, Origin, PromptRequest, ResponseError,
    session::SessionId,
};
use nanocodex_durability::{
    BranchBoundary, BranchPoint, DurableAgentExt, DurableSession, SessionRecord, SessionStore,
    TranscriptItem,
};

/// Answers every generation with a numbered reply.
#[derive(Clone)]
struct Replies(Arc<AtomicUsize>);

impl tower::Service<nanocodex_oai_api::tower::ResponsesAttempt> for Replies {
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
                let reply = format!("reply {}", self.0.fetch_add(1, Ordering::SeqCst) + 1);
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

fn user_prompts(transcript: &[TranscriptItem]) -> Vec<&str> {
    transcript
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::User(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// Runs prompts `(request id, text)` on a durable Codex session.
async fn turns(
    replies: &Arc<AtomicUsize>,
    workspace: &std::path::Path,
    durability: DurableSession,
    prompts: &[(&str, &str)],
) -> Result<()> {
    let replies = Arc::clone(replies);
    let client = OpenAi::builder("test-key")
        .service(move || Replies(Arc::clone(&replies)))
        .build()?;
    let (agent, _events) = Nanocodex::builder(client)
        .model(Model::Luna)
        .workspace(workspace)
        .durability(durability)
        .await?
        .build()?;
    for (id, text) in prompts {
        agent
            .prompt(PromptRequest::new(*text).request_id(*id))
            .await?
            .result()
            .await?;
    }
    agent.shutdown().await?;
    Ok(())
}

/// A latest branch keeps the source turns settled when it was created: later
/// source turns never join it, and editing the branch's first own prompt
/// resumes from the source checkpoint at that recorded boundary.
#[tokio::test]
async fn latest_branch_is_pinned_at_creation_and_edits_resolve_in_its_source() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let replies = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let root_id = SessionId::default().to_string();
    let root = SessionRecord::root(
        root_id.clone(),
        HarnessModel::Codex(Model::Luna),
        Some(workspace.clone()),
    );
    turns(
        &replies,
        &workspace,
        store.session(root.clone()).await?,
        &[("root-1", "first question"), ("root-2", "second question")],
    )
    .await?;

    let latest = store.branch(&root_id, BranchPoint::Latest, None).await?;
    let branch_id = latest.record.session_id.clone();
    let pinned = Some(BranchBoundary {
        source_session_id: root_id.clone(),
        through_turn: Some("root-2".to_owned()),
    });
    assert_eq!(latest.record.branch, pinned);

    // The source moves on after the branch was created.
    turns(
        &replies,
        &workspace,
        store.resume(&root_id).await?,
        &[("root-3", "later source question")],
    )
    .await?;
    turns(
        &replies,
        &workspace,
        store.resume(&branch_id).await?,
        &[("branch-1", "branch question")],
    )
    .await?;
    let branch = store.load(&branch_id).await?;
    assert_eq!(
        branch.summary.record.branch, pinned,
        "the boundary never moves"
    );
    assert_eq!(
        user_prompts(&branch.transcript),
        ["first question", "second question", "branch question"]
    );

    // Editing the branch's first own prompt starts from the source's
    // checkpoint at the boundary, not an empty conversation and not the
    // source's current latest turn.
    let edit = store
        .branch(&branch_id, BranchPoint::Before("branch-1".into()), None)
        .await?;
    assert_eq!(edit.record.lineage.origin, Origin::Branch);
    assert_eq!(
        edit.record.branch,
        Some(BranchBoundary {
            source_session_id: branch_id.clone(),
            through_turn: None,
        })
    );
    assert_eq!(
        user_prompts(&store.load(&edit.record.session_id).await?.transcript),
        ["first question", "second question"]
    );

    // A derived session saved without a boundary cannot prove what it
    // inherited: branching before its first turn fails instead of guessing.
    let legacy_id = SessionId::default().to_string();
    turns(
        &replies,
        &workspace,
        store
            .session(root.derive(legacy_id.clone(), Origin::Fork))
            .await?,
        &[("legacy-1", "legacy question")],
    )
    .await?;
    assert!(
        store
            .load(&legacy_id)
            .await?
            .summary
            .record
            .branch
            .is_none()
    );
    assert!(matches!(
        store
            .branch(&legacy_id, BranchPoint::Before("legacy-1".into()), None)
            .await,
        Err(nanocodex_durability::Error::InvalidState(_))
    ));
    Ok(())
}

/// Editing the first own prompt of a fork or side conversation continues the
/// history it inherited when it was created, never later parent turns.
#[tokio::test]
async fn fork_and_side_first_prompt_edits_keep_their_inherited_history() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let replies = Arc::new(AtomicUsize::new(0));
    let store = SessionStore::open(home.path())?;
    let root_id = SessionId::default().to_string();
    let client = {
        let replies = Arc::clone(&replies);
        OpenAi::builder("test-key")
            .service(move || Replies(Arc::clone(&replies)))
            .build()?
    };
    let (root, _events) = Nanocodex::builder(client)
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
    for (id, text) in [("root-1", "first question"), ("root-2", "second question")] {
        root.prompt(PromptRequest::new(text).request_id(id))
            .await?
            .result()
            .await?;
    }
    let (fork, _fork_events) = root.fork(ForkRequest::latest()).await?;
    let (side, _side_events) = root.fork(ForkRequest::latest().side_conversation()).await?;
    for (child, id, text) in [
        (&fork, "fork-1", "fork question"),
        (&side, "side-1", "side question"),
    ] {
        child
            .prompt(PromptRequest::new(text).request_id(id))
            .await?
            .result()
            .await?;
    }
    root.prompt(PromptRequest::new("later root question").request_id("root-3"))
        .await?
        .result()
        .await?;
    let children = [
        (fork.session_id().to_owned(), "fork-1"),
        (side.session_id().to_owned(), "side-1"),
    ];
    fork.shutdown().await?;
    side.shutdown().await?;
    root.shutdown().await?;
    for (child_id, first) in children {
        let edit = store
            .branch(&child_id, BranchPoint::Before(first.into()), None)
            .await?;
        assert_eq!(
            edit.record.branch,
            Some(BranchBoundary {
                source_session_id: child_id.clone(),
                through_turn: None,
            })
        );
        assert_eq!(
            user_prompts(&store.load(&edit.record.session_id).await?.transcript),
            ["first question", "second question"],
            "edit of {first} keeps the inherited history only"
        );
    }
    Ok(())
}
