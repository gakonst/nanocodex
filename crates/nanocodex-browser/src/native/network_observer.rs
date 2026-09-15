use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use chromiumoxide::{
    Connection, Method,
    cdp::{
        browser_protocol::{
            network::{
                EnableParams as NetworkEnableParams, GetRequestPostDataParams,
                GetRequestPostDataReturns, GetResponseBodyParams, GetResponseBodyReturns,
            },
            page::{
                GetFrameTreeParams, GetFrameTreeReturns, NavigateParams, NavigateReturns,
                ReloadParams, ReloadReturns, StopLoadingParams, StopLoadingReturns,
            },
            target::{
                FilterEntry, SessionId, SetAutoAttachParams, SetAutoAttachReturns, TargetFilter,
                TargetId,
            },
        },
        events::{CdpEvent, CdpEventMessage},
        js_protocol::runtime::RunIfWaitingForDebuggerParams,
    },
    error::CdpError,
    types::{CallId, Message},
};
use futures_util::StreamExt;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinHandle,
    time::timeout,
};
use tracing::warn;

use super::{
    BrowserError, BrowserNetworkBodyKind, BrowserNetworkContext, BrowserNetworkRequest,
    BrowserWebSocketDirection, Diagnostics, NetworkSource, apply_response, finish_request,
    network_headers, network_initiator, seconds_to_milliseconds,
};

const INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_CAPACITY: usize = 32;

pub(super) struct NetworkObserver {
    commands: mpsc::Sender<ObserverCommand>,
    task: JoinHandle<()>,
}

pub(super) struct NetworkBody {
    pub(super) body: String,
    pub(super) base64_encoded: bool,
}

enum ObserverCommand {
    Activate {
        target_id: String,
        response: oneshot::Sender<Result<(), String>>,
    },
    Body {
        session_id: String,
        request_id: String,
        kind: BrowserNetworkBodyKind,
        response: oneshot::Sender<Result<NetworkBody, String>>,
    },
    Page {
        target_id: String,
        command: ObserverPageCommand,
        response: oneshot::Sender<Result<ObserverPageCommandOutcome, ObserverPageCommandError>>,
    },
}

#[derive(Clone, Copy)]
enum ObserverPageCommandKind {
    Navigate,
    Reload,
    MainDocument,
    StopLoading,
}

enum ObserverPageCommand {
    Navigate(String),
    Reload(String),
    MainDocument,
    StopLoading,
}

impl ObserverPageCommand {
    const fn kind(&self) -> ObserverPageCommandKind {
        match self {
            Self::Navigate(_) => ObserverPageCommandKind::Navigate,
            Self::Reload(_) => ObserverPageCommandKind::Reload,
            Self::MainDocument => ObserverPageCommandKind::MainDocument,
            Self::StopLoading => ObserverPageCommandKind::StopLoading,
        }
    }
}

pub(super) struct NavigateOutcome {
    pub(super) frame_id: String,
    pub(super) loader_id: Option<String>,
    pub(super) error_text: Option<String>,
    pub(super) is_download: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MainDocument {
    pub(super) frame_id: String,
    pub(super) loader_id: String,
    pub(super) url: String,
    pub(super) unreachable_url: Option<String>,
}

enum ObserverPageCommandOutcome {
    Navigate(NavigateOutcome),
    Reload,
    MainDocument(MainDocument),
    StopLoading,
}

pub(super) enum ObserverPageCommandError {
    Rejected(String),
    Observer(String),
}

struct PendingBody {
    kind: BrowserNetworkBodyKind,
    response: oneshot::Sender<Result<NetworkBody, String>>,
}

struct PendingPageCommand {
    kind: ObserverPageCommandKind,
    response: oneshot::Sender<Result<ObserverPageCommandOutcome, ObserverPageCommandError>>,
}

#[derive(Default)]
struct AttachedTargets {
    root_session: Option<String>,
    child_sessions: HashSet<String>,
    configured_targets: HashSet<String>,
    session_targets: HashMap<String, String>,
    session_roots: HashMap<String, String>,
    activation_waiters: HashMap<String, Vec<oneshot::Sender<Result<(), String>>>>,
}

impl NetworkObserver {
    pub(super) async fn activate(&self, target_id: String) -> Result<(), BrowserError> {
        let (response, activated) = oneshot::channel();
        self.commands
            .send(ObserverCommand::Activate {
                target_id,
                response,
            })
            .await
            .map_err(|_| BrowserError::NetworkObserver {
                message: "the observer task stopped".to_owned(),
            })?;
        timeout(INITIALIZATION_TIMEOUT, activated)
            .await
            .map_err(|_| BrowserError::NetworkObserver {
                message: "target activation timed out".to_owned(),
            })?
            .map_err(|_| BrowserError::NetworkObserver {
                message: "the observer dropped an activation response".to_owned(),
            })?
            .map_err(|message| BrowserError::NetworkObserver { message })
    }

    pub(super) async fn body(
        &self,
        session_id: String,
        request_id: String,
        kind: BrowserNetworkBodyKind,
    ) -> Result<NetworkBody, BrowserError> {
        let (response, body) = oneshot::channel();
        self.commands
            .send(ObserverCommand::Body {
                session_id,
                request_id,
                kind,
                response,
            })
            .await
            .map_err(|_| BrowserError::NetworkObserver {
                message: "the observer task stopped".to_owned(),
            })?;
        body.await
            .map_err(|_| BrowserError::NetworkObserver {
                message: "the observer dropped a body response".to_owned(),
            })?
            .map_err(|message| BrowserError::NetworkObserver { message })
    }

    pub(super) async fn navigate(
        &self,
        target_id: String,
        url: String,
    ) -> Result<NavigateOutcome, ObserverPageCommandError> {
        match self
            .page_command(target_id, ObserverPageCommand::Navigate(url))
            .await?
        {
            ObserverPageCommandOutcome::Navigate(outcome) => Ok(outcome),
            _ => Err(ObserverPageCommandError::Observer(
                "the observer returned the wrong navigate response".to_owned(),
            )),
        }
    }

    pub(super) async fn reload(
        &self,
        target_id: String,
        loader_id: String,
    ) -> Result<(), ObserverPageCommandError> {
        match self
            .page_command(target_id, ObserverPageCommand::Reload(loader_id))
            .await?
        {
            ObserverPageCommandOutcome::Reload => Ok(()),
            _ => Err(ObserverPageCommandError::Observer(
                "the observer returned the wrong reload response".to_owned(),
            )),
        }
    }

    pub(super) async fn main_document(
        &self,
        target_id: String,
    ) -> Result<MainDocument, ObserverPageCommandError> {
        match self
            .page_command(target_id, ObserverPageCommand::MainDocument)
            .await?
        {
            ObserverPageCommandOutcome::MainDocument(document) => Ok(document),
            _ => Err(ObserverPageCommandError::Observer(
                "the observer returned the wrong main-document response".to_owned(),
            )),
        }
    }

    pub(super) async fn stop_loading(
        &self,
        target_id: String,
    ) -> Result<(), ObserverPageCommandError> {
        match self
            .page_command(target_id, ObserverPageCommand::StopLoading)
            .await?
        {
            ObserverPageCommandOutcome::StopLoading => Ok(()),
            _ => Err(ObserverPageCommandError::Observer(
                "the observer returned the wrong stop-loading response".to_owned(),
            )),
        }
    }

    async fn page_command(
        &self,
        target_id: String,
        command: ObserverPageCommand,
    ) -> Result<ObserverPageCommandOutcome, ObserverPageCommandError> {
        let (response, completed) = oneshot::channel();
        self.commands
            .send(ObserverCommand::Page {
                target_id,
                command,
                response,
            })
            .await
            .map_err(|_| {
                ObserverPageCommandError::Observer("the observer task stopped".to_owned())
            })?;
        completed.await.map_err(|_| {
            ObserverPageCommandError::Observer(
                "the observer dropped a page command response".to_owned(),
            )
        })?
    }

    pub(super) fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    pub(super) fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for NetworkObserver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(super) async fn start(
    websocket_address: &str,
    target_id: TargetId,
    diagnostics: Arc<StdMutex<Diagnostics>>,
) -> Result<NetworkObserver, BrowserError> {
    let mut connection = Connection::<CdpEventMessage>::connect(websocket_address).await?;
    let filter = TargetFilter::new(vec![
        FilterEntry::builder().r#type("page").exclude(false).build(),
        FilterEntry::builder().exclude(true).build(),
    ]);
    let auto_attach = SetAutoAttachParams::builder()
        .auto_attach(true)
        .wait_for_debugger_on_start(true)
        .flatten(true)
        .filter(filter)
        .build()
        .map_err(|message| BrowserError::NetworkObserver { message })?;
    let attach_call = submit(&mut connection, None, auto_attach)?;
    let (commands, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (ready, ready_rx) = oneshot::channel();
    let task = tokio::spawn(run(
        connection,
        attach_call,
        target_id,
        command_rx,
        ready,
        diagnostics,
    ));
    match timeout(INITIALIZATION_TIMEOUT, ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(NetworkObserver { commands, task }),
        Ok(Ok(Err(message))) => {
            task.abort();
            Err(BrowserError::NetworkObserver { message })
        }
        Ok(Err(_)) => {
            task.abort();
            Err(BrowserError::NetworkObserver {
                message: "the observer stopped during initialization".to_owned(),
            })
        }
        Err(_) => {
            task.abort();
            Err(BrowserError::NetworkObserver {
                message: "initialization timed out".to_owned(),
            })
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one select loop owns CDP ordering, activation handshakes, body routing, and shutdown"
)]
async fn run(
    mut connection: Connection<CdpEventMessage>,
    attach_call: CallId,
    root_target_id: TargetId,
    mut commands: mpsc::Receiver<ObserverCommand>,
    ready: oneshot::Sender<Result<(), String>>,
    diagnostics: Arc<StdMutex<Diagnostics>>,
) {
    let mut stop_reason = "the observer task stopped".to_owned();
    let mut ready = Some(ready);
    let mut pending_bodies = HashMap::<CallId, PendingBody>::new();
    let mut pending_page_commands = HashMap::<CallId, PendingPageCommand>::new();
    let mut targets = AttachedTargets::default();
    let mut active_target_id = root_target_id.as_ref().to_owned();
    let mut auto_attach_ready = false;

    loop {
        pending_bodies.retain(|_, pending| !pending.response.is_closed());
        pending_page_commands.retain(|_, pending| !pending.response.is_closed());
        tokio::select! {
            message = connection.next() => {
                let Some(message) = message else {
                    stop_reason = "the DevTools connection closed".to_owned();
                    fail_ready(&mut ready, &stop_reason);
                    break;
                };
                let message = match message {
                    Ok(message) => message,
                    Err(CdpError::InvalidMessage(_, error)) => {
                        warn!(
                            target: "nanocodex_browser",
                            %error,
                            "network observer skipped an unsupported DevTools message"
                        );
                        continue;
                    }
                    Err(error) => {
                        stop_reason = error.to_string();
                        fail_ready(&mut ready, &stop_reason);
                        warn!(target: "nanocodex_browser", %error, "network observer stopped");
                        break;
                    }
                };
                match message {
                    Message::Response(response) => {
                        if response.id == attach_call {
                            if let Err(message) = response_result::<SetAutoAttachReturns>(response) {
                                stop_reason.clone_from(&message);
                                fail_ready(&mut ready, &message);
                                break;
                            }
                            auto_attach_ready = true;
                            complete_ready(&mut ready, auto_attach_ready, &targets);
                        } else if let Some(pending) = pending_bodies.remove(&response.id) {
                            let result = decode_body_response(response, pending.kind);
                            let _ = pending.response.send(result);
                        } else if let Some(pending) = pending_page_commands.remove(&response.id) {
                            let result = decode_page_command_response(response, pending.kind);
                            let _ = pending.response.send(result);
                        } else if let Some(error) = response.error {
                            warn!(
                                target: "nanocodex_browser",
                                %error,
                                "child-target DevTools setup command failed"
                            );
                        }
                    }
                    Message::Event(event) => {
                        handle_event(
                            event,
                            &root_target_id,
                            &active_target_id,
                            &mut targets,
                            &mut connection,
                            &diagnostics,
                        );
                        complete_ready(&mut ready, auto_attach_ready, &targets);
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    break;
                };
                match command {
                    ObserverCommand::Activate {
                        target_id,
                        response,
                    } => {
                        active_target_id.clone_from(&target_id);
                        if targets.configured_targets.contains(&target_id) {
                            let _ = response.send(Ok(()));
                        } else {
                            targets
                                .activation_waiters
                                .entry(target_id)
                                .or_default()
                                .push(response);
                        }
                    }
                    ObserverCommand::Body {
                        session_id,
                        request_id,
                        kind,
                        response,
                    } => {
                        match submit_body(
                            &mut connection,
                            SessionId::new(session_id),
                            request_id,
                            kind,
                        ) {
                            Ok(call_id) => {
                                pending_bodies.insert(call_id, PendingBody { kind, response });
                            }
                            Err(message) => {
                                let _ = response.send(Err(message));
                            }
                        }
                    }
                    ObserverCommand::Page {
                        target_id,
                        command,
                        response,
                    } => {
                        let Some(session_id) = targets.session_targets.iter().find_map(
                            |(session_id, attached_target_id)| {
                                (attached_target_id == &target_id).then(|| session_id.clone())
                            },
                        ) else {
                            let _ = response.send(Err(ObserverPageCommandError::Observer(
                                format!("the page session for target {target_id} is unavailable"),
                            )));
                            continue;
                        };
                        let kind = command.kind();
                        match submit_page_command(
                            &mut connection,
                            SessionId::new(session_id),
                            command,
                        ) {
                            Ok(call_id) => {
                                pending_page_commands.insert(
                                    call_id,
                                    PendingPageCommand { kind, response },
                                );
                            }
                            Err(message) => {
                                let _ = response.send(Err(ObserverPageCommandError::Observer(message)));
                            }
                        }
                    }
                }
            }
        }
    }

    fail_ready(&mut ready, &stop_reason);
    for waiters in targets.activation_waiters.into_values() {
        for waiter in waiters {
            let _ = waiter.send(Err(stop_reason.clone()));
        }
    }
    for (_, pending) in pending_bodies {
        let _ = pending.response.send(Err(stop_reason.clone()));
    }
    for (_, pending) in pending_page_commands {
        let _ = pending
            .response
            .send(Err(ObserverPageCommandError::Observer(stop_reason.clone())));
    }
    while let Ok(command) = commands.try_recv() {
        match command {
            ObserverCommand::Activate { response, .. } => {
                let _ = response.send(Err(stop_reason.clone()));
            }
            ObserverCommand::Body { response, .. } => {
                let _ = response.send(Err(stop_reason.clone()));
            }
            ObserverCommand::Page { response, .. } => {
                let _ = response.send(Err(ObserverPageCommandError::Observer(stop_reason.clone())));
            }
        }
    }
}

fn configure_page(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
) -> Result<(), String> {
    enable_child_auto_attach(connection, session_id.clone())?;
    submit(
        connection,
        Some(session_id.clone()),
        NetworkEnableParams::default(),
    )
    .map_err(|error| error.to_string())?;
    submit(
        connection,
        Some(session_id),
        RunIfWaitingForDebuggerParams::default(),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn configure_root(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
) -> Result<(), String> {
    enable_child_auto_attach(connection, session_id.clone())?;
    resume_target(connection, session_id)
}

fn configure_worker(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
) -> Result<(), String> {
    enable_child_auto_attach(connection, session_id.clone())?;
    submit(
        connection,
        Some(session_id.clone()),
        NetworkEnableParams::default(),
    )
    .map_err(|error| error.to_string())?;
    resume_target(connection, session_id)
}

fn enable_child_auto_attach(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
) -> Result<(), String> {
    let params = SetAutoAttachParams::builder()
        .auto_attach(true)
        .wait_for_debugger_on_start(true)
        .flatten(true)
        .build()?;
    submit(connection, Some(session_id), params)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn resume_target(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
) -> Result<(), String> {
    submit(
        connection,
        Some(session_id),
        RunIfWaitingForDebuggerParams::default(),
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

fn submit<T: serde::Serialize + Method>(
    connection: &mut Connection<CdpEventMessage>,
    session_id: Option<SessionId>,
    command: T,
) -> Result<CallId, serde_json::Error> {
    connection.submit_command(
        command.identifier(),
        session_id,
        serde_json::to_value(command)?,
    )
}

fn submit_body(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
    request_id: String,
    kind: BrowserNetworkBodyKind,
) -> Result<CallId, String> {
    match kind {
        BrowserNetworkBodyKind::Request => submit(
            connection,
            Some(session_id),
            GetRequestPostDataParams::new(request_id),
        ),
        BrowserNetworkBodyKind::Response => submit(
            connection,
            Some(session_id),
            GetResponseBodyParams::new(request_id),
        ),
    }
    .map_err(|error| error.to_string())
}

fn submit_page_command(
    connection: &mut Connection<CdpEventMessage>,
    session_id: SessionId,
    command: ObserverPageCommand,
) -> Result<CallId, String> {
    match command {
        ObserverPageCommand::Navigate(url) => {
            submit(connection, Some(session_id), NavigateParams::new(url))
        }
        ObserverPageCommand::Reload(loader_id) => {
            let params = ReloadParams::builder().loader_id(loader_id).build();
            submit(connection, Some(session_id), params)
        }
        ObserverPageCommand::MainDocument => {
            submit(connection, Some(session_id), GetFrameTreeParams::default())
        }
        ObserverPageCommand::StopLoading => {
            submit(connection, Some(session_id), StopLoadingParams::default())
        }
    }
    .map_err(|error| error.to_string())
}

fn response_result<T: serde::de::DeserializeOwned>(
    response: chromiumoxide::types::Response,
) -> Result<T, String> {
    if let Some(error) = response.error {
        return Err(error.to_string());
    }
    serde_json::from_value(
        response
            .result
            .ok_or_else(|| "DevTools returned no command result".to_owned())?,
    )
    .map_err(|error| error.to_string())
}

fn decode_body_response(
    response: chromiumoxide::types::Response,
    kind: BrowserNetworkBodyKind,
) -> Result<NetworkBody, String> {
    match kind {
        BrowserNetworkBodyKind::Request => {
            let response = response_result::<GetRequestPostDataReturns>(response)?;
            Ok(NetworkBody {
                body: response.post_data,
                base64_encoded: false,
            })
        }
        BrowserNetworkBodyKind::Response => {
            let response = response_result::<GetResponseBodyReturns>(response)?;
            Ok(NetworkBody {
                body: response.body,
                base64_encoded: response.base64_encoded,
            })
        }
    }
}

fn decode_page_command_response(
    response: chromiumoxide::types::Response,
    kind: ObserverPageCommandKind,
) -> Result<ObserverPageCommandOutcome, ObserverPageCommandError> {
    if let Some(error) = response.error {
        return Err(ObserverPageCommandError::Rejected(error.to_string()));
    }
    let result = response.result.ok_or_else(|| {
        ObserverPageCommandError::Observer("DevTools returned no command result".to_owned())
    })?;
    match kind {
        ObserverPageCommandKind::Navigate => {
            let response = serde_json::from_value::<NavigateReturns>(result)
                .map_err(|error| ObserverPageCommandError::Observer(error.to_string()))?;
            Ok(ObserverPageCommandOutcome::Navigate(NavigateOutcome {
                frame_id: response.frame_id.as_ref().to_owned(),
                loader_id: response
                    .loader_id
                    .map(|loader_id| loader_id.as_ref().to_owned()),
                error_text: response.error_text,
                is_download: response.is_download.unwrap_or(false),
            }))
        }
        ObserverPageCommandKind::Reload => {
            serde_json::from_value::<ReloadReturns>(result)
                .map_err(|error| ObserverPageCommandError::Observer(error.to_string()))?;
            Ok(ObserverPageCommandOutcome::Reload)
        }
        ObserverPageCommandKind::MainDocument => {
            let response = serde_json::from_value::<GetFrameTreeReturns>(result)
                .map_err(|error| ObserverPageCommandError::Observer(error.to_string()))?;
            let frame = response.frame_tree.frame;
            Ok(ObserverPageCommandOutcome::MainDocument(MainDocument {
                frame_id: frame.id.as_ref().to_owned(),
                loader_id: frame.loader_id.as_ref().to_owned(),
                url: frame.url,
                unreachable_url: frame.unreachable_url,
            }))
        }
        ObserverPageCommandKind::StopLoading => {
            serde_json::from_value::<StopLoadingReturns>(result)
                .map_err(|error| ObserverPageCommandError::Observer(error.to_string()))?;
            Ok(ObserverPageCommandOutcome::StopLoading)
        }
    }
}

fn fail_ready(ready: &mut Option<oneshot::Sender<Result<(), String>>>, message: &str) {
    if let Some(ready) = ready.take() {
        let _ = ready.send(Err(message.to_owned()));
    }
}

fn complete_ready(
    ready: &mut Option<oneshot::Sender<Result<(), String>>>,
    auto_attach_ready: bool,
    targets: &AttachedTargets,
) {
    if auto_attach_ready
        && targets.root_session.is_some()
        && let Some(ready) = ready.take()
    {
        let _ = ready.send(Ok(()));
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive typed dispatch preserves child-target network event ordering"
)]
fn handle_event(
    event: CdpEventMessage,
    root_target_id: &TargetId,
    active_target_id: &str,
    targets: &mut AttachedTargets,
    connection: &mut Connection<CdpEventMessage>,
    diagnostics: &Arc<StdMutex<Diagnostics>>,
) {
    let parent_session = event.session_id.clone();
    if let CdpEvent::TargetAttachedToTarget(attached) = &event.params {
        let session_id = attached.session_id.as_ref().to_owned();
        let target_id = attached.target_info.target_id.as_ref().to_owned();
        if targets.configured_targets.contains(&target_id) {
            if let Err(message) = resume_target(connection, attached.session_id.clone()) {
                warn!(
                    target: "nanocodex_browser",
                    target_type = %attached.target_info.r#type,
                    %message,
                    "failed to resume duplicate child-target attachment"
                );
            }
            return;
        }
        let is_root = attached.target_info.target_id == *root_target_id;
        let is_child = parent_session.as_ref().is_some_and(|parent| {
            targets.root_session.as_ref() == Some(parent) || targets.child_sessions.contains(parent)
        });
        let setup = if is_root {
            configure_root(connection, attached.session_id.clone())
        } else if attached.target_info.r#type == "page" {
            configure_page(connection, attached.session_id.clone())
        } else if is_child {
            configure_worker(connection, attached.session_id.clone())
        } else {
            resume_target(connection, attached.session_id.clone())
        };
        match setup {
            Ok(()) if is_root => {
                targets.root_session.replace(session_id.clone());
                targets.configured_targets.insert(target_id.clone());
                targets
                    .session_roots
                    .insert(session_id.clone(), target_id.clone());
                targets.session_targets.insert(session_id, target_id);
                complete_activation(targets, attached.target_info.target_id.as_ref());
            }
            Ok(()) if is_child || attached.target_info.r#type == "page" => {
                targets.child_sessions.insert(session_id.clone());
                targets.configured_targets.insert(target_id.clone());
                let root_target = parent_session
                    .as_ref()
                    .and_then(|parent| targets.session_roots.get(parent))
                    .cloned()
                    .unwrap_or_else(|| target_id.clone());
                targets
                    .session_roots
                    .insert(session_id.clone(), root_target);
                targets.session_targets.insert(session_id, target_id);
                complete_activation(targets, attached.target_info.target_id.as_ref());
            }
            Ok(()) => {}
            Err(message) => {
                fail_activation(targets, attached.target_info.target_id.as_ref(), &message);
                warn!(
                    target: "nanocodex_browser",
                    target_type = %attached.target_info.r#type,
                    %message,
                    "failed to configure child target"
                );
            }
        }
        return;
    }
    if let CdpEvent::TargetDetachedFromTarget(detached) = &event.params {
        let detached = detached.session_id.as_ref();
        targets.child_sessions.remove(detached);
        targets.session_roots.remove(detached);
        if let Some(target_id) = targets.session_targets.remove(detached) {
            targets.configured_targets.remove(&target_id);
        }
        if targets.root_session.as_deref() == Some(detached) {
            targets.root_session = None;
        }
        return;
    }

    let Some(session_id) = event.session_id else {
        return;
    };
    if targets
        .session_targets
        .get(&session_id)
        .is_some_and(|target_id| target_id == active_target_id)
        || targets
            .session_roots
            .get(&session_id)
            .is_none_or(|target_id| target_id != active_target_id)
        || !targets.child_sessions.contains(&session_id)
    {
        return;
    }
    let request_key = |request_id: &str| child_request_key(&session_id, request_id);

    let Ok(mut diagnostics) = diagnostics.lock() else {
        return;
    };
    match event.params {
        CdpEvent::NetworkRequestWillBeSent(event) => {
            let id = request_key(event.request_id.as_ref());
            let timestamp = *event.timestamp.inner();
            if let Some(redirect) = &event.redirect_response
                && let Some(entry) = diagnostics.request_entry_mut(&id)
            {
                apply_response(&mut entry.request, redirect);
                finish_request(entry, timestamp, redirect.encoded_data_length);
            }
            diagnostics.push_request(
                &id,
                NetworkSource::ChildTarget {
                    session_id,
                    request_id: event.request_id.as_ref().to_owned(),
                },
                timestamp,
                BrowserNetworkRequest {
                    sequence: 0,
                    request_id: String::new(),
                    context: BrowserNetworkContext::ChildTarget,
                    body_available: true,
                    url: event.request.url.clone(),
                    method: event.request.method.clone(),
                    document_url: event.document_url.clone(),
                    resource_type: event
                        .r#type
                        .as_ref()
                        .map_or_else(|| "Other".to_owned(), |kind| kind.as_ref().to_owned()),
                    started_at_epoch_ms: seconds_to_milliseconds(*event.wall_time.inner()),
                    duration_ms: None,
                    initiator: Some(network_initiator(&event.initiator)),
                    request_headers: network_headers(&event.request.headers),
                    has_post_data: event.request.has_post_data.unwrap_or(false),
                    status: None,
                    status_text: None,
                    response_headers: Vec::new(),
                    mime_type: None,
                    charset: None,
                    protocol: None,
                    remote_ip_address: None,
                    remote_port: None,
                    from_disk_cache: false,
                    from_service_worker: false,
                    encoded_data_length: None,
                    timing: None,
                    completed: false,
                    failure: None,
                },
            );
        }
        CdpEvent::NetworkResponseReceived(event) => {
            let id = request_key(event.request_id.as_ref());
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                apply_response(&mut entry.request, &event.response);
                event
                    .r#type
                    .as_ref()
                    .clone_into(&mut entry.request.resource_type);
            }
        }
        CdpEvent::NetworkLoadingFinished(event) => {
            let id = request_key(event.request_id.as_ref());
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                finish_request(entry, *event.timestamp.inner(), event.encoded_data_length);
            }
        }
        CdpEvent::NetworkLoadingFailed(event) => {
            let id = request_key(event.request_id.as_ref());
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                entry.request.failure = Some(event.error_text.clone());
                event
                    .r#type
                    .as_ref()
                    .clone_into(&mut entry.request.resource_type);
                finish_request(entry, *event.timestamp.inner(), 0.0);
            }
        }
        CdpEvent::NetworkWebSocketCreated(event) => {
            let id = request_key(event.request_id.as_ref());
            if diagnostics.request_entry_mut(&id).is_none() {
                diagnostics.push_request(
                    &id,
                    NetworkSource::ChildTarget {
                        session_id,
                        request_id: event.request_id.as_ref().to_owned(),
                    },
                    0.0,
                    BrowserNetworkRequest {
                        sequence: 0,
                        request_id: String::new(),
                        context: BrowserNetworkContext::ChildTarget,
                        body_available: false,
                        url: event.url.clone(),
                        method: "GET".to_owned(),
                        document_url: String::new(),
                        resource_type: "WebSocket".to_owned(),
                        started_at_epoch_ms: 0,
                        duration_ms: None,
                        initiator: event.initiator.as_ref().map(network_initiator),
                        request_headers: Vec::new(),
                        has_post_data: false,
                        status: None,
                        status_text: None,
                        response_headers: Vec::new(),
                        mime_type: None,
                        charset: None,
                        protocol: Some("websocket".to_owned()),
                        remote_ip_address: None,
                        remote_port: None,
                        from_disk_cache: false,
                        from_service_worker: false,
                        encoded_data_length: None,
                        timing: None,
                        completed: false,
                        failure: None,
                    },
                );
            }
        }
        CdpEvent::NetworkWebSocketWillSendHandshakeRequest(event) => {
            let id = request_key(event.request_id.as_ref());
            let timestamp = *event.timestamp.inner();
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                entry.started_at_monotonic_seconds = timestamp;
                entry.request.started_at_epoch_ms =
                    seconds_to_milliseconds(*event.wall_time.inner());
                entry.request.request_headers = network_headers(&event.request.headers);
            }
        }
        CdpEvent::NetworkWebSocketHandshakeResponseReceived(event) => {
            let id = request_key(event.request_id.as_ref());
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                entry.request.status = Some(event.response.status);
                entry.request.status_text = Some(event.response.status_text.clone());
                entry.request.response_headers = network_headers(&event.response.headers);
            }
        }
        CdpEvent::NetworkWebSocketFrameSent(event) => {
            diagnostics.push_web_socket_message(
                request_key(event.request_id.as_ref()),
                BrowserWebSocketDirection::Sent,
                *event.timestamp.inner(),
                &event.response,
            );
        }
        CdpEvent::NetworkWebSocketFrameReceived(event) => {
            diagnostics.push_web_socket_message(
                request_key(event.request_id.as_ref()),
                BrowserWebSocketDirection::Received,
                *event.timestamp.inner(),
                &event.response,
            );
        }
        CdpEvent::NetworkWebSocketFrameError(event) => {
            let id = request_key(event.request_id.as_ref());
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                entry.request.failure = Some(event.error_message);
            }
        }
        CdpEvent::NetworkWebSocketClosed(event) => {
            let id = request_key(event.request_id.as_ref());
            if let Some(entry) = diagnostics.request_entry_mut(&id) {
                finish_request(entry, *event.timestamp.inner(), 0.0);
            }
        }
        _ => {}
    }
}

fn complete_activation(targets: &mut AttachedTargets, target_id: &str) {
    if let Some(waiters) = targets.activation_waiters.remove(target_id) {
        for waiter in waiters {
            let _ = waiter.send(Ok(()));
        }
    }
}

fn fail_activation(targets: &mut AttachedTargets, target_id: &str, message: &str) {
    if let Some(waiters) = targets.activation_waiters.remove(target_id) {
        for waiter in waiters {
            let _ = waiter.send(Err(message.to_owned()));
        }
    }
}

fn child_request_key(session_id: &str, request_id: &str) -> String {
    format!("child:{session_id}:{request_id}")
}
