use crate::{Error, Result};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub type Item = BTreeMap<String, Vec<u8>>;
pub trait Pasteboard {
    fn generation(&self) -> i64;
    fn snapshot(&self) -> Result<Vec<Item>>;
    /// Preserve the caller's ownership token through preparation to the last
    /// pre-mutation check. None means ownership was lost before any write.
    /// Return the count acquired by this write, never a later reader's count.
    /// Native check/clear operations need not be atomic across processes.
    fn install(&mut self, items: &[Item], expected: i64) -> Result<Option<i64>>;
    fn consumed(&self) -> bool;
    fn poll(&mut self, duration: Duration);
}

/// Save representations, wait for consumption and restore only while still owner.
/// Preserve newer writers at every checkable boundary, including cleanup.
/// The provider documents any irreducible native check-versus-write race.
pub struct Transaction<'a, B: Pasteboard> {
    board: &'a mut B,
    saved: Vec<Item>,
    owned: i64,
    restored: bool,
}
impl<'a, B: Pasteboard> Transaction<'a, B> {
    pub fn begin(board: &'a mut B, items: &[Item]) -> Result<Self> {
        let before = board.generation();
        let saved = board.snapshot()?;
        if before != board.generation() {
            return Err(Error::action("Clipboard changed while taking snapshot"));
        }
        let owned = board
            .install(items, before)?
            .ok_or_else(|| Error::action("Clipboard changed before installation"))?;
        if board.generation() != owned {
            return Err(Error::action("Clipboard changed after installation"));
        }
        Ok(Self {
            board,
            saved,
            owned,
            restored: false,
        })
    }
    pub fn wait(&mut self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        loop {
            if self.board.consumed() {
                return Ok(());
            }
            if self.board.generation() != self.owned {
                return Err(Error::action("Clipboard changed before consumption"));
            }
            if start.elapsed() >= timeout {
                return Err(Error::action(
                    "Timed out waiting for the application to read the clipboard",
                ));
            }
            self.board
                .poll(Duration::from_millis(10).min(timeout.saturating_sub(start.elapsed())));
        }
    }
    pub fn finish(mut self) -> Result<bool> {
        // Do not retry a failed restoration in Drop; that could overwrite a writer
        // which runs during an ambiguous OS write failure.
        self.restored = true;
        self.board
            .install(&self.saved, self.owned)
            .map(|receipt| receipt.is_some())
    }
}
impl<B: Pasteboard> Drop for Transaction<'_, B> {
    fn drop(&mut self) {
        if !self.restored {
            let _ = self.board.install(&self.saved, self.owned);
        }
    }
}
