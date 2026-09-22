//! Bounded wall-clock pacing for 8 kHz G.711 capture, before WebRTC encoding.
use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::Instant;

const FRAME_BYTES: usize = 160; // 20 ms at 8 kHz, one byte per sample.
const MAX_BUFFERED_BYTES: usize = 16_000; // Two seconds; fail rather than drop speech.
const FRAME_DURATION: Duration = Duration::from_millis(20);

pub struct CaptureQueue {
    bytes: VecDeque<u8>,
    deadline: Instant,
}
impl Default for CaptureQueue {
    fn default() -> Self {
        Self {
            bytes: VecDeque::new(),
            deadline: Instant::now(),
        }
    }
}
impl CaptureQueue {
    pub fn enqueue(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        if bytes.len() > MAX_BUFFERED_BYTES.saturating_sub(self.bytes.len()) {
            return Err("capture backlog exceeded");
        }
        self.bytes.extend(bytes);
        Ok(())
    }
    pub fn has_frame(&self) -> bool {
        self.bytes.len() >= FRAME_BYTES
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn pop_frame(&mut self) -> Vec<u8> {
        assert!(self.has_frame());
        self.bytes.drain(..FRAME_BYTES).collect()
    }
    pub fn sent(&mut self) {
        self.sent_at(Instant::now());
    }
    fn sent_at(&mut self, now: Instant) {
        // Preserve the clock cadence instead of accumulating timer wake-up jitter.
        // If a whole period was missed, restart rather than burst to catch up.
        self.deadline += FRAME_DURATION;
        if self.deadline <= now {
            self.deadline = now + FRAME_DURATION;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_is_explicit_and_preserves_already_queued_speech() {
        let mut queue = CaptureQueue::default();
        queue.enqueue(&vec![7; MAX_BUFFERED_BYTES]).unwrap();
        assert!(queue.enqueue(&[8]).is_err());
        for _ in 0..100 {
            assert_eq!(queue.pop_frame(), vec![7; FRAME_BYTES]);
        }
        assert!(!queue.has_frame());
    }

    #[test]
    fn partial_input_frames_are_reassembled_without_padding_or_drops() {
        let mut queue = CaptureQueue::default();
        queue.enqueue(&[1; 79]).unwrap();
        assert!(!queue.has_frame());
        queue.enqueue(&[2; 82]).unwrap();
        assert_eq!(queue.pop_frame(), [vec![1; 79], vec![2; 81]].concat());
        assert!(!queue.has_frame());
        queue.enqueue(&[3; 159]).unwrap();
        assert_eq!(queue.pop_frame(), [vec![2], vec![3; 159]].concat());
    }

    #[tokio::test]
    async fn one_second_burst_is_fifty_paced_frames_and_controls_remain_responsive() {
        let mut queue = CaptureQueue::default();
        queue.enqueue(&[0xff; 8000]).unwrap();
        let start = Instant::now();
        let control = tokio::time::sleep(Duration::from_millis(50));
        tokio::pin!(control);
        let mut control_seen = false;
        let mut times = Vec::new();
        while queue.has_frame() {
            tokio::select! {
                _ = &mut control, if !control_seen => {
                    control_seen = true;
                    // Control runs during capture, not after the whole second drains.
                    assert!(times.len() < 50);
                }
                _ = tokio::time::sleep_until(queue.deadline()) => {
                    assert_eq!(queue.pop_frame(), vec![0xff; FRAME_BYTES]);
                    times.push(Instant::now());
                    queue.sent();
                }
            }
        }
        assert!(control_seen);
        assert_eq!(times.len(), 50);
        assert!(times[49].duration_since(start) >= Duration::from_millis(980));
    }

    #[test]
    fn normal_timer_jitter_does_not_accumulate_capture_backlog() {
        let mut queue = CaptureQueue::default();
        let start = queue.deadline();
        for frame in 0..1000 {
            let scheduled = start + FRAME_DURATION * frame;
            // Normal 20ms input arrivals, with 1ms wake-up jitter each time.
            queue.enqueue(&[0xff; FRAME_BYTES]).unwrap();
            assert_eq!(queue.pop_frame(), vec![0xff; FRAME_BYTES]);
            assert!(!queue.has_frame());
            queue.sent_at(scheduled + Duration::from_millis(1));
            assert_eq!(queue.deadline(), scheduled + FRAME_DURATION);
        }
        assert_eq!(queue.deadline(), start + Duration::from_secs(20));
    }

    #[test]
    fn late_submission_does_not_schedule_catch_up_packets() {
        let mut queue = CaptureQueue::default();
        let late = queue.deadline() + Duration::from_millis(45);
        queue.sent_at(late);
        assert_eq!(queue.deadline(), late + FRAME_DURATION);
        queue.sent_at(late + FRAME_DURATION + Duration::from_millis(1));
        assert_eq!(queue.deadline(), late + FRAME_DURATION * 2);
    }
}
