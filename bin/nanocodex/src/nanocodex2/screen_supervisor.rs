//! Screen-only recovery. A published session is retained across capture failures:
//! creating a new publisher here would override another host's replacement fence.
use std::{future::Future, time::Duration};

pub(super) trait Session {
    type Error: std::fmt::Display;
    fn is_finished(&self) -> bool;
    /// Repair capture in place. Return true only when a helper was restarted.
    fn maintain(&mut self) -> impl Future<Output = Result<bool, Self::Error>>;
    fn shutdown(self) -> impl Future<Output = Result<(), Self::Error>>;
}

const POLL: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);

pub(super) async fn while_attached<S: Session, F: Future<Output = Result<S, S::Error>>>(
    start: impl FnMut() -> F,
    attachment: impl Future<Output = Result<(), S::Error>>,
) -> Result<(), S::Error> {
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let screen = supervise(start, async {
        let _ = stopped.await;
    });
    let hand = async {
        let result = attachment.await;
        // Shutdown, authentication failure and attachment fencing all stop capture.
        let _ = stop.send(());
        result
    };
    let (result, stopped) = tokio::join!(hand, screen);
    result.and(stopped)
}

pub(super) async fn supervise<S: Session, F: Future<Output = Result<S, S::Error>>>(
    mut start: impl FnMut() -> F,
    shutdown: impl Future<Output = ()>,
) -> Result<(), S::Error> {
    tokio::pin!(shutdown);
    let mut retry = POLL;
    let mut screen = loop {
        let result = tokio::select! {
            biased;
            () = &mut shutdown => return Ok(()),
            result = start() => result,
        };
        match result {
            Ok(screen) => break screen,
            Err(error) => {
                tracing::warn!(target: "nanocodex2", stage = "native.screen.unavailable", %error,
                retry_ms = retry.as_millis() as u64,
                "Native screen unavailable; shell and filesystem remain connected")
            }
        }
        tokio::select! {
            biased;
            () = &mut shutdown => return Ok(()),
            () = tokio::time::sleep(retry) => {},
        }
        retry = (retry * 2).min(MAX_RETRY);
    };
    tracing::info!(target: "nanocodex2", stage = "native.screen.ready", "Native screen is ready");
    retry = POLL;
    let mut delay = POLL;
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => return screen.shutdown().await,
            () = tokio::time::sleep(delay) => {},
        }
        // Terminal publication (including Host replaced) wins over helper death.
        // Never call start again after obtaining a published session.
        if screen.is_finished() {
            tracing::info!(target: "nanocodex2", stage = "native.screen.stopped", "Native screen publisher stopped");
            return screen.shutdown().await;
        }
        let result = tokio::select! {
            biased;
            () = &mut shutdown => return screen.shutdown().await,
            result = screen.maintain() => result,
        };
        delay = match result {
            Ok(recovered) => {
                if recovered {
                    tracing::info!(target: "nanocodex2", stage = "native.screen.recovered", "Native screen capture recovered");
                } else {
                    retry = POLL;
                }
                POLL
            }
            Err(error) => {
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
}
