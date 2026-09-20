//! Small, session-owned preparation set: dropping/removing an entry cancels its
//! future immediately. No spawned tasks or completion queue can outlive an ID.
use futures_util::future::{BoxFuture, poll_fn};
use std::{future::Future, task::Poll};
use tokio::time::{Instant, error::Elapsed, timeout_at};

pub const VIEWER_CAPACITY: usize = 4;
type Completion<T> = (String, Instant, Result<T, Elapsed>);

type Entry<T> = (String, Instant, BoxFuture<'static, Result<T, Elapsed>>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    Capacity,
    Duplicate,
}
impl std::fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capacity => "viewer preparation capacity reached",
            Self::Duplicate => "viewer preparation already exists",
        })
    }
}
impl std::error::Error for AdmissionError {}

pub struct Preparations<T> {
    entries: Vec<Entry<T>>,
}
impl<T: Send + 'static> Default for Preparations<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T: Send + 'static> Preparations<T> {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn contains(&self, id: &str) -> bool {
        self.entries.iter().any(|(viewer, _, _)| viewer == id)
    }
    pub fn insert(
        &mut self,
        id: &str,
        deadline: Instant,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Result<(), AdmissionError> {
        if self.len() >= VIEWER_CAPACITY {
            return Err(AdmissionError::Capacity);
        }
        if self.contains(id) {
            return Err(AdmissionError::Duplicate);
        }
        self.entries
            .push((id.into(), deadline, Box::pin(timeout_at(deadline, future))));
        Ok(())
    }
    pub fn remove(&mut self, id: &str) {
        if let Some(index) = self.entries.iter().position(|(viewer, _, _)| viewer == id) {
            drop(self.entries.swap_remove(index));
        }
    }
    // Cancellation-safe when another select! branch wins: only a returned
    // completion removes an entry. At most four futures are polled per wake.
    pub async fn next(&mut self) -> Completion<T> {
        poll_fn(|cx| {
            for index in 0..self.entries.len() {
                if let Poll::Ready(result) = self.entries[index].2.as_mut().poll(cx) {
                    let (id, deadline, _) = self.entries.swap_remove(index);
                    return Poll::Ready((id, deadline, result));
                }
            }
            Poll::Pending
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::{pending, ready},
        time::Duration,
    };
    use tokio::sync::oneshot;

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(8)
    }

    #[tokio::test]
    async fn stalled_preparation_does_not_block_events_or_other_completions() {
        let mut preparations = Preparations::new();
        preparations
            .insert("stalled", deadline(), pending::<u8>())
            .unwrap();
        // Force the pending branch to be polled before an established input
        // event wins, cancelling only this next() call, not its owned future.
        tokio::select! {
            biased;
            _ = preparations.next() => panic!("stalled preparation completed"),
            _ = ready(()) => {},
        }
        assert!(preparations.contains("stalled"));
        preparations.insert("fast", deadline(), ready(7)).unwrap();
        let (id, _, result) = preparations.next().await;
        assert_eq!((id.as_str(), result.unwrap()), ("fast", 7));
        assert!(preparations.contains("stalled"));
    }

    #[tokio::test]
    async fn removal_cancels_polled_future_and_id_reuse_cannot_receive_old_completion() {
        let mut preparations = Preparations::new();
        let (old, old_result) = oneshot::channel();
        preparations.insert("v", deadline(), old_result).unwrap();
        tokio::select! {
            biased;
            _ = preparations.next() => panic!("not ready"),
            _ = ready(()) => {},
        }
        preparations.remove("v");
        assert!(
            old.send(1).is_err(),
            "cancelled future must be dropped immediately"
        );
        let (new, new_result) = oneshot::channel();
        preparations.insert("v", deadline(), new_result).unwrap();
        new.send(2).unwrap();
        let (id, _, result) = preparations.next().await;
        assert_eq!(id, "v");
        assert_eq!(result.unwrap().unwrap(), 2);
        // Also cancel a ready but not yet consumed result before reuse.
        let (old, old_result) = oneshot::channel();
        preparations.insert("v", deadline(), old_result).unwrap();
        old.send(3).unwrap();
        preparations.remove("v");
        preparations.insert("v", deadline(), ready(Ok(4))).unwrap();
        assert_eq!(preparations.next().await.2.unwrap().unwrap(), 4);
    }

    #[tokio::test]
    async fn session_drop_cancels_every_future_even_before_polling() {
        let mut preparations = Preparations::new();
        let mut senders = Vec::new();
        for id in ["a", "b", "c", "d"] {
            let (sender, receiver) = oneshot::channel::<()>();
            preparations.insert(id, deadline(), receiver).unwrap();
            senders.push(sender);
        }
        drop(preparations);
        assert!(senders.into_iter().all(|sender| sender.send(()).is_err()));
    }

    #[tokio::test]
    async fn capacity_and_duplicates_reject_without_replacing_owned_work() {
        let mut preparations = Preparations::new();
        let (sender, receiver) = oneshot::channel::<()>();
        preparations.insert("a", deadline(), receiver).unwrap();
        assert!(preparations.insert("a", deadline(), pending()).is_err());
        assert!(!sender.is_closed());
        for id in ["b", "c", "d"] {
            preparations.insert(id, deadline(), pending()).unwrap();
        }
        assert!(preparations.insert("e", deadline(), pending()).is_err());
        assert_eq!(preparations.len(), VIEWER_CAPACITY);
        preparations.remove("b");
        preparations.insert("e", deadline(), pending()).unwrap();
        sender.send(()).unwrap();
        assert_eq!(preparations.next().await.0, "a");
    }

    #[tokio::test]
    async fn timeout_drops_future_and_preserves_original_setup_deadline() {
        let mut preparations = Preparations::new();
        let (sender, receiver) = oneshot::channel::<()>();
        let expires = Instant::now() + Duration::from_millis(10);
        preparations.insert("v", expires, receiver).unwrap();
        let (id, setup_deadline, result) =
            tokio::time::timeout(Duration::from_secs(1), preparations.next())
                .await
                .unwrap();
        assert_eq!(id, "v");
        assert_eq!(setup_deadline, expires);
        assert!(result.is_err());
        assert!(sender.is_closed());
        assert_eq!(preparations.len(), 0);
    }

    #[tokio::test]
    async fn failed_preparation_completes_without_poisoning_other_viewers() {
        let mut preparations = Preparations::new();
        preparations
            .insert("bad", deadline(), ready(Err::<(), _>("invalid ICE")))
            .unwrap();
        preparations
            .insert("good", deadline(), ready(Ok(())))
            .unwrap();
        assert_eq!(preparations.next().await.2.unwrap(), Err("invalid ICE"));
        assert_eq!(preparations.next().await.2.unwrap(), Ok(()));
    }
}
