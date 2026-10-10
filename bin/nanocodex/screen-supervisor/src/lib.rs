//! Screen-only recovery. A published session is retained across capture failures:
//! creating a new publisher here would override another host's replacement fence.
use std::{future::Future, time::Duration};

pub trait Session {
    type Error: std::fmt::Display;
    fn is_finished(&self) -> bool;
    /// Whether the published session is connected now; a session that is
    /// reconnecting is not ready even while capture works.
    fn is_connected(&self) -> bool {
        true
    }
    /// Whether owned capture infrastructure visibly needs repair.
    fn capture_lost(&mut self) -> bool {
        false
    }
    /// Repair capture in place. Return true only when a helper was restarted.
    fn maintain(&mut self) -> impl Future<Output = Result<bool, Self::Error>>;
    fn shutdown(self) -> impl Future<Output = Result<(), Self::Error>>;
}

const POLL: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);

#[cfg(test)]
pub async fn while_attached<S: Session, F: Future<Output = Result<S, S::Error>>>(
    start: impl FnMut() -> F,
    attachment: impl Future<Output = Result<(), S::Error>>,
) -> Result<(), S::Error> {
    while_attached_observed(start, attachment, |_| {}).await
}

/// Screen lifecycle for status readers. Ready means capture works and the
/// session was published; later viewer reconnects of a published session are
/// not reported. Stopped is terminal for this attachment: a finished publisher
/// (for example, replaced by another host) is never started again.
#[derive(Debug)]
pub enum Report<'a, E> {
    Starting,
    Ready,
    Unavailable(&'a E),
    /// Capture works, but the published session is lost and reconnecting.
    Reconnecting,
    /// Owned capture infrastructure died and is being repaired.
    Recovering,
    Stopped(Stop),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The publisher ended, including a replacement fence.
    Finished,
    /// The attachment or process shut the screen down.
    Shutdown,
}

pub async fn while_attached_observed<S: Session, F: Future<Output = Result<S, S::Error>>>(
    start: impl FnMut() -> F,
    attachment: impl Future<Output = Result<(), S::Error>>,
    mut observe: impl FnMut(Option<&S::Error>),
) -> Result<(), S::Error> {
    while_attached_reported(start, attachment, move |report| match report {
        Report::Ready => observe(None),
        Report::Unavailable(error) => observe(Some(error)),
        Report::Starting | Report::Reconnecting | Report::Recovering | Report::Stopped(_) => {}
    })
    .await
}

pub async fn while_attached_reported<S: Session, F: Future<Output = Result<S, S::Error>>>(
    start: impl FnMut() -> F,
    attachment: impl Future<Output = Result<(), S::Error>>,
    report: impl FnMut(Report<'_, S::Error>),
) -> Result<(), S::Error> {
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let screen = supervise_reported(
        start,
        async {
            let _ = stopped.await;
        },
        report,
    );
    let hand = async {
        let result = attachment.await;
        // Shutdown, authentication failure and attachment fencing all stop capture.
        let _ = stop.send(());
        result
    };
    let (result, stopped) = tokio::join!(hand, screen);
    result.and(stopped)
}

#[cfg(test)]
pub async fn supervise<S: Session, F: Future<Output = Result<S, S::Error>>>(
    start: impl FnMut() -> F,
    shutdown: impl Future<Output = ()>,
) -> Result<(), S::Error> {
    supervise_reported(start, shutdown, |_| {}).await
}

/// None marks a usable publication/capture; Some reports retryable capture
/// failure. A terminal replacement never becomes ready or starts again.
pub async fn supervise_observed<S: Session, F: Future<Output = Result<S, S::Error>>>(
    start: impl FnMut() -> F,
    shutdown: impl Future<Output = ()>,
    mut observe: impl FnMut(Option<&S::Error>),
) -> Result<(), S::Error> {
    supervise_reported(start, shutdown, move |report| match report {
        Report::Ready => observe(None),
        Report::Unavailable(error) => observe(Some(error)),
        Report::Starting | Report::Reconnecting | Report::Recovering | Report::Stopped(_) => {}
    })
    .await
}

pub async fn supervise_reported<S: Session, F: Future<Output = Result<S, S::Error>>>(
    mut start: impl FnMut() -> F,
    shutdown: impl Future<Output = ()>,
    mut report: impl FnMut(Report<'_, S::Error>),
) -> Result<(), S::Error> {
    tokio::pin!(shutdown);
    report(Report::Starting);
    let mut retry = POLL;
    let mut screen = loop {
        let result = tokio::select! {
            biased;
            () = &mut shutdown => {
                report(Report::Stopped(Stop::Shutdown));
                return Ok(());
            }
            result = start() => result,
        };
        match result {
            Ok(screen) => break screen,
            Err(error) => {
                report(Report::Unavailable(&error));
                tracing::warn!(target: "nanocodex2", stage = "native.screen.unavailable", %error,
                retry_ms = retry.as_millis() as u64,
                "Native screen unavailable; shell and filesystem remain connected")
            }
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                report(Report::Stopped(Stop::Shutdown));
                return Ok(());
            }
            () = tokio::time::sleep(retry) => {},
        }
        retry = (retry * 2).min(MAX_RETRY);
    };
    if screen.is_finished() {
        report(Report::Stopped(Stop::Finished));
        return screen.shutdown().await;
    }
    // Every report reads the session now, never a value cached across an await.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Shown {
        Ready,
        Reconnecting,
        Recovering,
        Unavailable,
    }
    let mut shown: Option<Shown> = None;
    macro_rules! healthy {
        () => {{
            let next = if screen.is_connected() {
                Shown::Ready
            } else {
                Shown::Reconnecting
            };
            if shown != Some(next) {
                shown = Some(next);
                report(if next == Shown::Ready {
                    Report::Ready
                } else {
                    Report::Reconnecting
                });
            }
        }};
    }
    healthy!();
    tracing::info!(target: "nanocodex2", stage = "native.screen.ready", "Native screen is ready");
    retry = POLL;
    let mut delay = POLL;
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                report(Report::Stopped(Stop::Shutdown));
                return screen.shutdown().await;
            }
            () = tokio::time::sleep(delay) => {},
        }
        // Terminal publication (including Host replaced) wins over helper death.
        // Never call start again after obtaining a published session.
        if screen.is_finished() {
            tracing::info!(target: "nanocodex2", stage = "native.screen.stopped", "Native screen publisher stopped");
            report(Report::Stopped(Stop::Finished));
            return screen.shutdown().await;
        }
        if shown != Some(Shown::Unavailable) {
            if screen.capture_lost() {
                if shown != Some(Shown::Recovering) {
                    shown = Some(Shown::Recovering);
                    report(Report::Recovering);
                }
            } else if shown != Some(Shown::Recovering) {
                healthy!();
            }
        }
        let result = tokio::select! {
            biased;
            () = &mut shutdown => {
                report(Report::Stopped(Stop::Shutdown));
                return screen.shutdown().await;
            }
            result = screen.maintain() => result,
        };
        // The session may have been fenced or lost while capture was repaired.
        if screen.is_finished() {
            tracing::info!(target: "nanocodex2", stage = "native.screen.stopped", "Native screen publisher stopped");
            report(Report::Stopped(Stop::Finished));
            return screen.shutdown().await;
        }
        delay = match result {
            Ok(recovered) => {
                if recovered {
                    // A completed repair is announced again even when the
                    // visible state did not change, as before.
                    shown = None;
                    tracing::info!(target: "nanocodex2", stage = "native.screen.recovered", "Native screen capture recovered");
                } else {
                    retry = POLL;
                }
                healthy!();
                POLL
            }
            Err(error) => {
                shown = Some(Shown::Unavailable);
                report(Report::Unavailable(&error));
                tracing::warn!(target: "nanocodex2", stage = "native.screen.recovery_failed", %error,
                    retry_ms = retry.as_millis() as u64, "Native screen capture recovery failed");
                let delay = retry;
                retry = (retry * 2).min(MAX_RETRY);
                delay
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::{
        sync::{Notify, oneshot},
        time::Instant,
    };

    #[derive(Default)]
    struct State {
        finished: AtomicBool,
        block_recovery: AtomicBool,
        failures: AtomicUsize,
        attempts: Mutex<Vec<Instant>>,
        maintained: Notify,
        stops: AtomicUsize,
    }
    struct Screen(Arc<State>);
    impl Session for Screen {
        type Error = &'static str;
        fn is_finished(&self) -> bool {
            self.0.finished.load(Ordering::SeqCst)
        }
        async fn maintain(&mut self) -> Result<bool, Self::Error> {
            self.0.attempts.lock().unwrap().push(Instant::now());
            self.0.maintained.notify_one();
            if self.0.block_recovery.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if self
                .0
                .failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                .is_ok()
            {
                Err("display unavailable")
            } else {
                Ok(true)
            }
        }
        async fn shutdown(self) -> Result<(), Self::Error> {
            self.0.stops.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    async fn stopped(rx: oneshot::Receiver<()>) {
        let _ = rx.await;
    }

    #[tokio::test(start_paused = true)]
    async fn startup_retries_are_capped_and_recover_without_another_session() {
        let times = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(State::default());
        let (stop, rx) = oneshot::channel();
        let (ready, waiting) = oneshot::channel();
        let mut ready = Some(ready);
        let attempts = times.clone();
        let session = state.clone();
        let began = Instant::now();
        let worker = tokio::spawn(supervise(
            move || {
                let attempt = {
                    let mut times = attempts.lock().unwrap();
                    times.push(Instant::now());
                    times.len()
                };
                let result = if attempt < 9 {
                    Err("display unavailable")
                } else {
                    ready.take().unwrap().send(()).unwrap();
                    Ok(Screen(session.clone()))
                };
                std::future::ready(result)
            },
            stopped(rx),
        ));
        waiting.await.unwrap();
        let times: Vec<_> = times
            .lock()
            .unwrap()
            .iter()
            .map(|at| (*at - began).as_secs())
            .collect();
        assert_eq!(times, [0, 1, 3, 7, 15, 31, 61, 91, 121]);
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
        assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_during_retry_does_not_start_again() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let began = Arc::new(Notify::new());
        let (stop, rx) = oneshot::channel();
        let count = attempts.clone();
        let signal = began.clone();
        let worker = tokio::spawn(supervise(
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                signal.notify_one();
                std::future::ready(Err::<Screen, _>("display unavailable"))
            },
            stopped(rx),
        ));
        began.notified().await;
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_publisher_wins_over_capture_failure() {
        let state = Arc::new(State::default());
        state.finished.store(true, Ordering::SeqCst);
        state.failures.store(10, Ordering::SeqCst);
        let session = state.clone();
        let mut starts = 0;
        supervise(
            || {
                starts += 1;
                std::future::ready(Ok(Screen(session.clone())))
            },
            std::future::pending(),
        )
        .await
        .unwrap();
        assert_eq!(starts, 1);
        assert!(state.attempts.lock().unwrap().is_empty());
        assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn replacement_after_publication_skips_capture_repair() {
        let state = Arc::new(State::default());
        state.failures.store(10, Ordering::SeqCst);
        let starts = Arc::new(AtomicUsize::new(0));
        let session = state.clone();
        let count = starts.clone();
        let (ready, waiting) = oneshot::channel();
        let mut ready = Some(ready);
        let worker = tokio::spawn(supervise_observed(
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(Screen(session.clone())))
            },
            std::future::pending(),
            move |error| {
                assert!(error.is_none());
                ready.take().unwrap().send(()).unwrap();
            },
        ));
        waiting.await.unwrap();
        state.finished.store(true, Ordering::SeqCst);
        worker.await.unwrap().unwrap();
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(state.attempts.lock().unwrap().is_empty());
        assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn capture_retry_keeps_the_publisher_and_cancels_pending_recovery() {
        let state = Arc::new(State::default());
        state.failures.store(2, Ordering::SeqCst);
        let starts = Arc::new(AtomicUsize::new(0));
        let (stop, rx) = oneshot::channel();
        let session = state.clone();
        let count = starts.clone();
        let began = Instant::now();
        let worker = tokio::spawn(supervise(
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Ok(Screen(session.clone())))
            },
            stopped(rx),
        ));
        for _ in 0..3 {
            state.maintained.notified().await;
        }
        let times: Vec<_> = state
            .attempts
            .lock()
            .unwrap()
            .iter()
            .map(|at| (*at - began).as_secs())
            .collect();
        assert_eq!(times, [1, 2, 4]);
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        state.block_recovery.store(true, Ordering::SeqCst);
        state.maintained.notified().await;
        stop.send(()).unwrap();
        worker.await.unwrap().unwrap();
        assert_eq!(state.stops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn attachment_failure_cancels_startup_without_waiting_for_display() {
        struct Cleanup(Arc<AtomicUsize>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let began = Arc::new(Notify::new());
        let signal = began.clone();
        let count = dropped.clone();
        let result = while_attached(
            move || {
                let guard = Cleanup(count.clone());
                let signal = signal.clone();
                async move {
                    let _guard = guard;
                    signal.notify_one();
                    std::future::pending::<Result<Screen, &'static str>>().await
                }
            },
            async {
                // Represents an independently running attachment: it can make progress
                // while screen startup is stuck, and a fence promptly drops that startup.
                began.notified().await;
                Err("attachment fenced")
            },
        )
        .await;
        assert_eq!(result, Err("attachment fenced"));
        assert_eq!(dropped.load(Ordering::SeqCst), 1);
    }

    fn label<E>(report: &Report<'_, E>) -> &'static str {
        match report {
            Report::Starting => "starting",
            Report::Ready => "ready",
            Report::Unavailable(_) => "unavailable",
            Report::Reconnecting => "reconnecting",
            Report::Recovering => "recovering",
            Report::Stopped(Stop::Finished) => "stopped:finished",
            Report::Stopped(Stop::Shutdown) => "stopped:shutdown",
        }
    }

    /// Status readers see each attachment start, publication, and the terminal
    /// stop; a finished publisher is never reported ready again.
    #[tokio::test(start_paused = true)]
    async fn reports_starting_ready_and_terminal_stops() {
        for (finished_before_ready, shutdown, expected) in [
            (false, false, vec!["starting", "ready", "stopped:finished"]),
            (true, false, vec!["starting", "stopped:finished"]),
            (false, true, vec!["starting", "ready", "stopped:shutdown"]),
        ] {
            let state = Arc::new(State::default());
            state
                .finished
                .store(finished_before_ready, Ordering::SeqCst);
            let session = state.clone();
            let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = reports.clone();
            let (ready, waiting) = oneshot::channel();
            let mut ready = Some(ready);
            let (stop, stopped) = oneshot::channel::<()>();
            let worker = tokio::spawn(supervise_reported(
                move || std::future::ready(Ok(Screen(session.clone()))),
                async move {
                    let _ = stopped.await;
                },
                move |report| {
                    let label = label(&report);
                    seen.lock().unwrap().push(label);
                    if label != "starting"
                        && let Some(ready) = ready.take()
                    {
                        let _ = ready.send(());
                    }
                },
            ));
            waiting.await.unwrap();
            if shutdown {
                stop.send(()).unwrap();
            } else {
                state.finished.store(true, Ordering::SeqCst);
            }
            worker.await.unwrap().unwrap();
            assert_eq!(*reports.lock().unwrap(), expected);
        }
    }

    struct Link(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
    );
    impl Session for Link {
        type Error = &'static str;
        fn is_finished(&self) -> bool {
            self.1.load(Ordering::SeqCst)
        }
        fn is_connected(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        async fn maintain(&mut self) -> Result<bool, Self::Error> {
            Ok(false)
        }
        async fn shutdown(self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// A lost published session is reported as reconnecting, never ready, and
    /// becomes ready again only once it is connected.
    #[tokio::test(start_paused = true)]
    async fn reports_reconnecting_until_the_session_is_connected_again() {
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = reports.clone();
        let (link, done) = (connected.clone(), finished.clone());
        let worker = tokio::spawn(supervise_reported(
            move || std::future::ready(Ok(Link(link.clone(), done.clone()))),
            std::future::pending(),
            move |report| seen.lock().unwrap().push(label(&report)),
        ));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        connected.store(false, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(5)).await;
        connected.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(2)).await;
        finished.store(true, Ordering::SeqCst);
        worker.await.unwrap().unwrap();
        assert_eq!(
            *reports.lock().unwrap(),
            vec![
                "starting",
                "ready",
                "reconnecting",
                "ready",
                "stopped:finished"
            ]
        );
    }

    struct Repair(
        Arc<std::sync::atomic::AtomicBool>,
        Arc<std::sync::atomic::AtomicBool>,
    );
    impl Session for Repair {
        type Error = &'static str;
        fn is_finished(&self) -> bool {
            self.1.load(Ordering::SeqCst)
        }
        fn capture_lost(&mut self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
        async fn maintain(&mut self) -> Result<bool, Self::Error> {
            Ok(self.0.swap(false, Ordering::SeqCst))
        }
        async fn shutdown(self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// Visible capture loss reports recovering before the repair, then ready.
    #[tokio::test(start_paused = true)]
    async fn reports_recovering_while_capture_is_repaired() {
        let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = reports.clone();
        let (capture, done) = (lost.clone(), finished.clone());
        let worker = tokio::spawn(supervise_reported(
            move || std::future::ready(Ok(Repair(capture.clone(), done.clone()))),
            std::future::pending(),
            move |report| seen.lock().unwrap().push(label(&report)),
        ));
        tokio::time::sleep(Duration::from_millis(1500)).await;
        lost.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(3)).await;
        finished.store(true, Ordering::SeqCst);
        worker.await.unwrap().unwrap();
        assert_eq!(
            *reports.lock().unwrap(),
            vec![
                "starting",
                "ready",
                "recovering",
                "ready",
                "stopped:finished"
            ]
        );
    }

    /// Capture repair during which the link drops (fence = false) or the
    /// publisher is fenced (fence = true).
    struct Racy {
        lost: Arc<std::sync::atomic::AtomicBool>,
        connected: Arc<std::sync::atomic::AtomicBool>,
        finished: Arc<std::sync::atomic::AtomicBool>,
        fence: bool,
    }
    impl Session for Racy {
        type Error = &'static str;
        fn is_finished(&self) -> bool {
            self.finished.load(Ordering::SeqCst)
        }
        fn is_connected(&self) -> bool {
            self.connected.load(Ordering::SeqCst)
        }
        fn capture_lost(&mut self) -> bool {
            self.lost.load(Ordering::SeqCst)
        }
        async fn maintain(&mut self) -> Result<bool, Self::Error> {
            if !self.lost.swap(false, Ordering::SeqCst) {
                return Ok(false);
            }
            if self.fence {
                self.finished.store(true, Ordering::SeqCst);
            } else {
                self.connected.store(false, Ordering::SeqCst);
            }
            Ok(true)
        }
        async fn shutdown(self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    /// State that changes while a repair is awaited is read afresh: a dropped
    /// link reports reconnecting and a fence stops, never a stale ready.
    #[tokio::test(start_paused = true)]
    async fn repair_never_reports_ready_from_state_cached_before_it() {
        for (fence, expected) in [
            (
                false,
                vec![
                    "starting",
                    "ready",
                    "recovering",
                    "reconnecting",
                    "stopped:finished",
                ],
            ),
            (
                true,
                vec!["starting", "ready", "recovering", "stopped:finished"],
            ),
        ] {
            let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
            let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let reports = Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = reports.clone();
            let (l, c, f) = (lost.clone(), connected.clone(), finished.clone());
            let worker = tokio::spawn(supervise_reported(
                move || {
                    std::future::ready(Ok(Racy {
                        lost: l.clone(),
                        connected: c.clone(),
                        finished: f.clone(),
                        fence,
                    }))
                },
                std::future::pending(),
                move |report| seen.lock().unwrap().push(label(&report)),
            ));
            tokio::time::sleep(Duration::from_millis(1500)).await;
            lost.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(3)).await;
            finished.store(true, Ordering::SeqCst);
            worker.await.unwrap().unwrap();
            assert_eq!(*reports.lock().unwrap(), expected, "fence={fence}");
        }
    }
}
