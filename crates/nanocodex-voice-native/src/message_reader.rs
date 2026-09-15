//! Read bounded helper frames independently of pipe chunks; cancellation retains partial input.

use std::io;

use tokio::sync::mpsc;

use crate::Message;
use crate::decode_frame;

// Matches the maximum read in codex_utils_pty's pipe output reader. Retain at most
// one such chunk and one bounded partial frame, never a queue of decoded messages.
const MAX_CHUNK_BYTES: usize = 8_192;

pub(crate) struct MessageReader {
    output: mpsc::Receiver<Vec<u8>>,
    pending: std::vec::IntoIter<u8>,
    frame: Vec<u8>,
}

impl MessageReader {
    pub(crate) fn new(output: mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            output,
            pending: Vec::new().into_iter(),
            frame: Vec::new(),
        }
    }

    pub(crate) async fn next(&mut self) -> io::Result<Message> {
        loop {
            let target = if let [a, b, c, d, ..] = self.frame.as_slice() {
                // Validate the header before consuming or allocating its payload.
                decode_frame(&self.frame)?;
                u32::from_be_bytes([*a, *b, *c, *d]) as usize + 4
            } else {
                4
            };
            while self.frame.len() < target {
                if self.pending.len() == 0 {
                    let chunk = self.output.recv().await.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "voice helper output closed")
                    })?;
                    if chunk.is_empty() || chunk.len() > MAX_CHUNK_BYTES {
                        return Err(io::Error::other("invalid voice helper output chunk"));
                    }
                    self.pending = chunk.into_iter();
                }
                self.frame
                    .extend(self.pending.by_ref().take(target - self.frame.len()));
            }
            if let Some(message) = decode_frame(&self.frame)? {
                self.frame.clear();
                return Ok(message);
            }
        }
    }
}

#[cfg(test)]
#[path = "message_reader_tests.rs"]
mod tests;
