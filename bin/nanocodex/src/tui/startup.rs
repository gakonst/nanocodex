//! The UI owns initialization and holds commands until the backend is ready.
use std::future::Future;

use super::*;

pub(super) struct Task<T> {
    task: Option<tokio::task::JoinHandle<T>>,
}

impl<T: Send + 'static> Task<T> {
    pub(super) fn spawn(future: impl Future<Output = T> + Send + 'static) -> Self {
        Self {
            task: Some(tokio::spawn(future)),
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        self.task.is_some()
    }

    pub(super) async fn finish(&mut self) -> Result<T, tokio::task::JoinError> {
        let result = self.task.as_mut().expect("initialization is pending").await;
        self.task = None;
        result
    }

    // A completed result can win the race with quit. Return it to the caller so
    // that fully constructed runtime resources still receive explicit shutdown.
    pub(super) async fn cancel(&mut self) -> Result<Option<T>, tokio::task::JoinError> {
        let Some(task) = self.task.take() else {
            return Ok(None);
        };
        task.abort();
        match task.await {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.is_cancelled() => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[derive(Default)]
pub(super) struct Commands(VecDeque<WorkerCommand>);

impl Commands {
    pub(super) fn drain(&mut self, app: &mut App, rx: &mut mpsc::UnboundedReceiver<WorkerCommand>) {
        while let Ok(command) = rx.try_recv() {
            match command {
                WorkerCommand::Cancel { target } => self.cancel(app, target),
                WorkerCommand::CloseBtw { id } => {
                    self.cancel(app, PaneId::Btw(id));
                    self.0.push_back(WorkerCommand::CloseBtw { id });
                }
                command => self.0.push_back(command),
            }
        }
    }

    fn cancel(&mut self, app: &mut App, target: PaneId) {
        const CANCELLED: &str = "Cancelled before initialization finished";
        let mut commands = std::mem::take(&mut self.0);
        while let Some(command) = commands.pop_front() {
            match command {
                WorkerCommand::Prompt {
                    target: pane,
                    prompt_id,
                    ..
                } if pane == target => {
                    app.reject_external(target, prompt_id, false, CANCELLED.to_owned());
                }
                WorkerCommand::Steer {
                    target: pane, id, ..
                } if pane == target => {
                    app.reject_external(target, id, true, CANCELLED.to_owned());
                }
                WorkerCommand::InterruptForSteers {
                    target: pane,
                    prompt_id,
                    steer_ids,
                    ..
                } if pane == target => {
                    app.reject_external(target, prompt_id, false, CANCELLED.to_owned());
                    for id in steer_ids {
                        app.reject_external(target, id, true, CANCELLED.to_owned());
                    }
                }
                WorkerCommand::OpenBtw { id, prompt_id, .. } if PaneId::Btw(id) == target => {
                    if let Some(prompt_id) = prompt_id {
                        app.reject_external(target, prompt_id, false, CANCELLED.to_owned());
                    }
                    self.0.push_back(WorkerCommand::OpenBtw {
                        id,
                        prompt_id: None,
                        prompt: None,
                    });
                }
                command => self.0.push_back(command),
            }
        }
        app.cancel_settled(target);
    }

    pub(super) fn flush(self, tx: &mpsc::UnboundedSender<WorkerCommand>) -> Result<()> {
        for command in self.0 {
            tx.send(command)?;
        }
        Ok(())
    }
}

pub(super) struct Backend {
    pub(super) configured: crate::config::ConfiguredAgent,
    pub(super) observability: Option<nanocodex_observability::ObservabilityGuard>,
    pub(super) cwd: PathBuf,
    pub(super) control_server: Option<nanocodex_tui_control::Server>,
}

impl Backend {
    pub(super) fn start(
        config: AgentArgs,
        vm: crate::vm::VmArgs,
        resume: Option<DurableSession>,
        observability: Option<crate::observability::ObservabilityArgs>,
    ) -> Task<Result<Self>> {
        Task::spawn(async move {
            let cwd = resume
                .as_ref()
                .map(|session| PathBuf::from(session.workspace()))
                .map_or_else(|| resolve_cwd(&config), Ok)?;
            let observability = observability
                .map(|args| args.install(true, &cwd))
                .transpose()?;
            if let Err(error) = crate::update::prepare_legacy_nightly_bootstrap() {
                tracing::warn!(%error, "failed to prepare the Nanocodex updater bootstrap");
            }
            if let Err(error) = crate::update::ensure_default_automatic_updates() {
                tracing::warn!(%error, "could not configure automatic updates");
            }
            let control_server = if nanocodex_tui_control::Server::enabled() {
                Some(nanocodex_tui_control::Server::start("native")?)
            } else {
                None
            };
            let configured = if let Some(session) = resume {
                config.build_resumed_tui(session, vm).await?
            } else {
                config.build_tui(vm).await?
            };
            Ok(Self {
                configured,
                observability,
                cwd,
                control_server,
            })
        })
    }

    pub(super) async fn shutdown(self) -> Result<()> {
        let configured = self.configured;
        drop((
            configured.handle,
            configured.events,
            configured.realtime,
            configured.subagent_updates,
            configured.mcp,
        ));
        shutdown_runtime(
            None,
            configured.child_agents,
            configured.mpp_adapter,
            configured.browser,
            configured.vm,
        )
        .await
    }
}

pub(super) async fn stop_backend(task: &mut Task<Result<Backend>>) -> Result<()> {
    if let Some(Ok(backend)) = task
        .cancel()
        .await
        .wrap_err("TUI initialization task failed")?
    {
        backend.shutdown().await?;
    }
    Ok(())
}

pub(super) fn display_renderer(
    profile: ratatex::TerminalProfile,
    on_update: impl Fn() + Send + Sync + 'static,
) -> Result<Option<Ratatex>> {
    // Unsupported terminals always use the source-text fallback. Starting the
    // renderer there only creates idle workers that must be joined on quit.
    if profile.graphics != ratatex::GraphicsSupport::Kitty {
        return Ok(None);
    }
    Ratatex::builder(profile)
        .on_update(on_update)
        .build()
        .map(Some)
        .wrap_err("failed to initialize the display-math renderer")
}

pub(super) async fn stop_display(task: &mut Task<Result<Option<Ratatex>>>) -> Result<()> {
    if let Some(Ok(Some(renderer))) = task
        .cancel()
        .await
        .wrap_err("TUI display initialization task failed")?
    {
        renderer.shutdown();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn unsupported_display_finishes_once_without_retaining_workers_or_callback() {
        let (tx, mut rx) = mpsc::channel(1);
        let mut task = Task::spawn(async move {
            display_renderer(
                ratatex::TerminalProfile::unsupported(Default::default()),
                move || {
                    let _ = tx.try_send(());
                },
            )
        });
        assert!(task.is_pending());
        assert!(task.finish().await.unwrap().unwrap().is_none());
        assert!(!task.is_pending());
        // With no graphics work possible, no worker owns the update callback.
        assert_eq!(rx.recv().await, None);
        stop_display(&mut task).await.unwrap();
    }

    #[tokio::test]
    async fn quit_accepts_a_completed_unsupported_display() {
        let mut task = Task::spawn(async {
            display_renderer(
                ratatex::TerminalProfile::unsupported(Default::default()),
                || {},
            )
        });
        while !task.task.as_ref().unwrap().is_finished() {
            tokio::task::yield_now().await;
        }
        stop_display(&mut task).await.unwrap();
        assert!(!task.is_pending());
    }

    #[tokio::test]
    async fn quit_drops_a_never_ready_initializer() {
        struct OnDrop(Arc<AtomicBool>);
        impl Drop for OnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let owned = OnDrop(Arc::clone(&dropped));
        let (entered, started) = tokio::sync::oneshot::channel();
        let mut task = Task::spawn(async move {
            let _owned = owned;
            let _ = entered.send(());
            std::future::pending::<()>().await;
        });
        started.await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), task.cancel())
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn completed_initialization_is_returned_for_cleanup_on_quit() {
        let mut task = Task::spawn(async { 42 });
        while !task.task.as_ref().unwrap().is_finished() {
            tokio::task::yield_now().await;
        }
        assert_eq!(task.cancel().await.unwrap(), Some(42));
    }

    #[tokio::test]
    async fn initialization_failure_is_preserved() {
        let mut task = Task::spawn(async { Err::<(), _>("unavailable") });
        assert_eq!(task.finish().await.unwrap(), Err("unavailable"));
        assert!(task.cancel().await.unwrap().is_none());
    }

    #[test]
    fn prompts_and_draft_survive_slow_startup_but_cancelled_prompts_are_never_dispatched()
    -> Result<()> {
        let mut app = App::new(PathBuf::from("/synthetic"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pending = Commands::default();
        submit_initial_prompt(
            &mut app,
            "",
            &tx,
            Some(InitialPrompt::plain("first".into())),
        )?;
        pending.drain(&mut app, &mut rx);
        app.input = "second".into();
        app.cursor = app.input.len();
        submit(&mut app, "", &tx, SubmitIntent::Immediate)?;
        tx.send(WorkerCommand::Cancel {
            target: PaneId::Main,
        })?;
        app.input = "third".into();
        app.cursor = app.input.len();
        submit(&mut app, "", &tx, SubmitIntent::Immediate)?;
        app.input = "editable draft".into();
        app.cursor = app.input.len();
        pending.drain(&mut app, &mut rx);
        assert_eq!(app.input, "editable draft");
        assert_eq!(app.main.pending_turns, 1);
        pending.flush(&tx)?;
        assert!(
            matches!(rx.try_recv(), Ok(WorkerCommand::Prompt { prompt, .. }) if prompt.display() == "third")
        );
        assert!(rx.try_recv().is_err());
        Ok(())
    }
    #[tokio::test]
    async fn composer_edits_and_quit_work_while_initialization_is_pending() -> Result<()> {
        let mut backend = Task::spawn(std::future::pending::<()>());
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut ui = UiModel::new(App::new(PathBuf::from("/synthetic")), Arc::from(""));
        for code in [
            KeyCode::Char('h'),
            KeyCode::Char('i'),
            KeyCode::Left,
            KeyCode::Char('!'),
        ] {
            assert_eq!(
                ui.update(
                    UiAction::Terminal(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))),
                    &tx
                )?,
                UiUpdate::Redraw(RedrawPriority::Immediate),
            );
        }
        assert_eq!(ui.app.input, "h!i");
        assert_eq!(ui.app.cursor, 2);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24))?;
        terminal.draw(|frame| view::render(frame, &mut ui.app))?;
        assert!(!backend.task.as_ref().unwrap().is_finished());
        assert_eq!(
            ui.update(
                UiAction::Terminal(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL
                ))),
                &tx
            )?,
            UiUpdate::Quit,
        );
        assert!(backend.cancel().await?.is_none());
        Ok(())
    }

    #[test]
    fn initial_workflow_is_dispatched_once_without_overwriting_a_new_draft() -> Result<()> {
        let mut app = App::new(PathBuf::from("/synthetic"));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut pending = Commands::default();
        submit_initial_prompt(
            &mut app,
            "",
            &tx,
            Some(InitialPrompt::workflow(
                "workflow".into(),
                "synthetic instruction".into(),
            )),
        )?;
        pending.drain(&mut app, &mut rx);
        app.input = "next draft".into();
        app.cursor = app.input.len();
        assert!(rx.try_recv().is_err());
        pending.flush(&tx)?;
        let WorkerCommand::Prompt { prompt, .. } = rx.try_recv()? else {
            panic!("expected initial prompt")
        };
        let mut expected = SubmittedPrompt::text("workflow".into());
        expected.set_instruction("synthetic instruction".into());
        assert_eq!(prompt, expected);
        assert_eq!(app.input, "next draft");
        assert!(rx.try_recv().is_err());
        Ok(())
    }
}
