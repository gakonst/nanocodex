//! Bounded external PCM source for the same libWebRTC mixer/ADM/APM as remote audio.
use codex_realtime_webrtc::{MAX_PCM_SAMPLES, PcmStatus};
use libwebrtc::native::audio_resampler::AudioResampler;
use std::{
    ffi::c_void,
    time::{Duration, Instant},
};

unsafe extern "C" {
    fn nanocodex_pcm_create() -> *mut c_void;
    fn nanocodex_pcm_destroy(handle: *mut c_void);
    fn nanocodex_pcm_begin(handle: *mut c_void, generation: u64) -> i32;
    fn nanocodex_pcm_write(
        handle: *mut c_void,
        generation: u64,
        data: *const i16,
        length: usize,
        finish: bool,
    ) -> i32;
    fn nanocodex_pcm_peak(handle: *mut c_void) -> u16;
    fn nanocodex_pcm_available(handle: *mut c_void) -> usize;
    fn nanocodex_pcm_status(handle: *mut c_void, generation: u64) -> i32;
    fn nanocodex_pcm_cancel(handle: *mut c_void, generation: u64) -> i32;
}

pub(super) struct Pcm {
    handle: *mut c_void,
    generation: u64,
    rate: u32,
    pending: Vec<i16>,
    resampler: AudioResampler,
    draining: bool,
    drained_at: Option<Instant>,
}
impl Pcm {
    /// Must immediately precede factory construction on the same thread.
    pub(super) fn new() -> Self {
        Self {
            handle: unsafe { nanocodex_pcm_create() },
            generation: 0,
            rate: 48000,
            pending: Vec::with_capacity(1440),
            resampler: AudioResampler::default(),
            draining: false,
            drained_at: None,
        }
    }
    pub(super) fn take_peak(&self) -> u16 {
        unsafe { nanocodex_pcm_peak(self.handle) }
    }

    pub(super) fn begin(&mut self, generation: u64, rate: u32) -> PcmStatus {
        if !matches!(rate, 16000 | 24000 | 48000) {
            return PcmStatus::Unsupported;
        }
        if unsafe { nanocodex_pcm_begin(self.handle, generation) } != 0 {
            return PcmStatus::Stale;
        }
        self.generation = generation;
        self.rate = rate;
        self.pending.clear();
        self.resampler = AudioResampler::default();
        self.draining = false;
        self.drained_at = None;
        PcmStatus::Ready
    }
    pub(super) fn write(&mut self, generation: u64, samples: &[i16]) -> PcmStatus {
        if generation != self.generation
            || self.draining
            || unsafe { nanocodex_pcm_status(self.handle, generation) } < 0
        {
            return PcmStatus::Stale;
        }
        if samples.is_empty() || samples.len() > MAX_PCM_SAMPLES {
            return PcmStatus::Unsupported;
        }
        let block = self.rate as usize / 100;
        let needed = (self.pending.len() + samples.len()) / block * 480;
        if unsafe { nanocodex_pcm_available(self.handle) } < needed {
            return PcmStatus::Busy;
        }
        self.pending.extend_from_slice(samples);
        while self.pending.len() >= block {
            let output = self.resampler.remix_and_resample(
                &self.pending[..block],
                block as u32,
                1,
                self.rate,
                1,
                48000,
            );
            let status = unsafe {
                nanocodex_pcm_write(
                    self.handle,
                    generation,
                    output.as_ptr(),
                    output.len(),
                    false,
                )
            };
            debug_assert_eq!(status, 0); // Single producer; callback can only free capacity.
            if status != 0 {
                return PcmStatus::Stale;
            }
            self.pending.drain(..block);
        }
        self.drained_at = None;
        PcmStatus::Ready
    }
    pub(super) fn drain(&mut self, generation: u64) -> PcmStatus {
        if generation != self.generation
            || unsafe { nanocodex_pcm_status(self.handle, generation) } < 0
        {
            return PcmStatus::Stale;
        }
        if !self.draining {
            // Flush the partial frame and sinc filter tail through the normal mixer.
            let block = self.rate as usize / 100;
            if unsafe { nanocodex_pcm_available(self.handle) } < 960 {
                return PcmStatus::Busy;
            }
            self.pending.resize(block * 2, 0);
            let mut tail = Vec::with_capacity(960);
            for input in self.pending.chunks_exact(block) {
                tail.extend_from_slice(&self.resampler.remix_and_resample(
                    input,
                    block as u32,
                    1,
                    self.rate,
                    1,
                    48000,
                ));
            }
            // Publish the resampler tail and EOF in one native operation.
            if unsafe {
                nanocodex_pcm_write(self.handle, generation, tail.as_ptr(), tail.len(), true)
            } != 0
            {
                return PcmStatus::Stale;
            }
            self.pending.clear();
            self.draining = true;
        }
        if unsafe { nanocodex_pcm_status(self.handle, generation) } != 0 {
            return PcmStatus::Busy;
        }
        let drained = self.drained_at.get_or_insert_with(Instant::now);
        if drained.elapsed() < Duration::from_millis(100) {
            PcmStatus::Busy
        } else {
            PcmStatus::Ready
        }
    }
    pub(super) fn cancel(&mut self, generation: u64) -> PcmStatus {
        if unsafe { nanocodex_pcm_cancel(self.handle, generation) } != 0 {
            return PcmStatus::Stale;
        }
        self.pending.clear();
        self.draining = true;
        PcmStatus::Ready
    }
}
impl Drop for Pcm {
    fn drop(&mut self) {
        unsafe { nanocodex_pcm_destroy(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" {
        fn nanocodex_pcm_test_render(handle: *mut c_void, data: *mut i16, capacity: usize)
        -> usize;
    }
    unsafe extern "C" {
        fn nanocodex_pcm_test_attached(handle: *mut c_void) -> bool;
    }
    #[test]
    fn repeated_factory_teardown_releases_mixer_after_success_and_cancel() {
        // Partial initialization must also release its thread-local pending state.
        drop(Pcm::new());
        for generation in 1..=3 {
            let mut pcm = Pcm::new();
            let factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
            assert!(unsafe { nanocodex_pcm_test_attached(pcm.handle) });
            assert_eq!(pcm.begin(generation, 48000), PcmStatus::Ready);
            assert_eq!(pcm.write(generation, &[5000; 480]), PcmStatus::Ready);
            if generation % 2 == 0 {
                assert_eq!(pcm.cancel(generation), PcmStatus::Ready);
            } else {
                let _ = render(&pcm);
            }
            drop(factory);
            let deadline = Instant::now() + Duration::from_secs(5);
            while unsafe { nanocodex_pcm_test_attached(pcm.handle) } {
                assert!(Instant::now() < deadline, "factory leaked its native mixer");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    fn render(pcm: &Pcm) -> [i16; 480] {
        let mut output = [0; 480];
        assert_eq!(
            unsafe { nanocodex_pcm_test_render(pcm.handle, output.as_mut_ptr(), output.len()) },
            480
        );
        output
    }
    // HTTP-like 40 ms bursts, alternating 70/10 ms delivery intervals.
    // Drive the attached production mixer on its real 10 ms cadence.
    #[test]
    fn paced_jittered_bursts_have_no_internal_silence_or_hard_edges() {
        check_paced_bursts(&[0, 7, 8, 15, 16, 23, 24, 31]);
        check_paced_bursts(&[0, 8, 9, 15, 16, 24, 25, 31]);
    }
    fn check_paced_bursts(arrivals: &[usize]) {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(99, 48000), PcmStatus::Ready);
        let mut blocks = Vec::new();
        for tick in 0..48 {
            if arrivals.contains(&tick) {
                for _ in 0..2 {
                    assert_eq!(pcm.write(99, &[12000; 960]), PcmStatus::Ready);
                }
            }
            blocks.push(render(&pcm));
            std::thread::sleep(Duration::from_millis(10));
        }
        let first = blocks
            .iter()
            .position(|b| b.iter().any(|s| *s != 0))
            .unwrap();
        let last = blocks
            .iter()
            .rposition(|b| b.iter().any(|s| *s != 0))
            .unwrap();
        let gaps = blocks[first..=last]
            .iter()
            .filter(|b| b.iter().all(|s| *s == 0))
            .count();
        let samples: Vec<_> = blocks.iter().flatten().copied().collect();
        let jump = samples
            .windows(2)
            .map(|w| (i32::from(w[1]) - i32::from(w[0])).abs())
            .max()
            .unwrap();
        eprintln!(
            "paced burst metrics: startup={}ms internal_silent_blocks={} max_step={}",
            first * 10,
            gaps,
            jump
        );
        assert_eq!(gaps, 0);
        assert!(first <= 6, "startup must be bounded at 60 ms");
        assert!(jump <= 100, "5 ms ramps should bound constant-signal steps");
    }
    #[test]
    fn mixer_consumes_external_pcm_and_cancel_fences_old_generations() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(1, 48000), PcmStatus::Ready);
        assert_eq!(pcm.write(1, &[12000; 480]), PcmStatus::Ready);
        for _ in 0..6 {
            assert!(render(&pcm).iter().all(|s| *s == 0));
        }
        let output = render(&pcm);
        assert_eq!(output[0], 0);
        assert_eq!(output[239], 11950);
        assert_eq!(output[479], 0);
        assert_eq!(pcm.take_peak(), 11950);
        assert_eq!(pcm.take_peak(), 0);
        assert_eq!(pcm.write(1, &[9000; 480]), PcmStatus::Ready);
        assert_eq!(pcm.cancel(1), PcmStatus::Ready);
        assert!(render(&pcm).iter().all(|sample| *sample == 0));
        assert_eq!(pcm.write(1, &[5000; 480]), PcmStatus::Stale);
        assert_eq!(pcm.begin(1, 48000), PcmStatus::Stale);
        assert_eq!(pcm.begin(2, 48000), PcmStatus::Ready);
        assert_eq!(pcm.cancel(1), PcmStatus::Stale);
        assert_eq!(pcm.write(2, &[6000; 480]), PcmStatus::Ready);
        for _ in 0..6 {
            assert!(render(&pcm).iter().all(|s| *s == 0));
        }
        assert_eq!(render(&pcm)[239], 5975);
    }
    #[test]
    fn render_continues_while_producer_is_preempted() {
        unsafe extern "C" {
            fn nanocodex_pcm_test_lock_producer(handle: *mut c_void);
            fn nanocodex_pcm_test_unlock_producer(handle: *mut c_void);
        }
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(101, 48000), PcmStatus::Ready);
        for _ in 0..5 {
            assert_eq!(pcm.write(101, &[12000; 960]), PcmStatus::Ready);
        }
        // Hold the producer lock over ten real callbacks, modeling preemption.
        // A callback locking this mutex would deadlock; try_lock would drop audio.
        unsafe { nanocodex_pcm_test_lock_producer(pcm.handle) };
        let started = Instant::now();
        let output: Vec<_> = (0..10)
            .map(|_| {
                let block = render(&pcm);
                std::thread::sleep(Duration::from_millis(10));
                block
            })
            .collect();
        unsafe { nanocodex_pcm_test_unlock_producer(pcm.handle) };
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(output.iter().all(|b| b.iter().any(|s| *s != 0)));
    }

    #[test]
    fn cancel_full_queue_then_begin_discards_only_retired_generation() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(102, 48000), PcmStatus::Ready);
        for _ in 0..10 {
            assert_eq!(pcm.write(102, &[12000; 960]), PcmStatus::Ready);
        }
        assert_eq!(pcm.cancel(102), PcmStatus::Ready);
        assert_eq!(pcm.begin(103, 48000), PcmStatus::Ready);
        assert_eq!(pcm.write(103, &[6000; 960]), PcmStatus::Busy);
        assert!(render(&pcm).iter().all(|s| *s == 0));
        for _ in 0..3 {
            assert_eq!(pcm.write(103, &[6000; 960]), PcmStatus::Ready);
        }
        let block = render(&pcm);
        assert_eq!(block[479], 6000);
        assert!(block.iter().all(|s| (0..=6000).contains(s)));
    }
    #[test]
    fn begin_before_callback_preserves_new_samples_behind_cancelled_samples() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        for generation in 1..=100 {
            assert_eq!(pcm.begin(generation * 2, 48000), PcmStatus::Ready);
            assert_eq!(pcm.write(generation * 2, &[12000; 960]), PcmStatus::Ready);
            assert_eq!(pcm.cancel(generation * 2), PcmStatus::Ready);
            assert_eq!(pcm.begin(generation * 2 + 1, 48000), PcmStatus::Ready);
            for _ in 0..3 {
                assert_eq!(
                    pcm.write(generation * 2 + 1, &[6000; 960]),
                    PcmStatus::Ready
                );
            }
            for _ in 0..6 {
                let block = render(&pcm);
                assert!(block.iter().any(|s| *s != 0));
                assert!(block.iter().all(|s| (0..=6000).contains(s)));
            }
        }
    }
    #[test]
    fn finish_fades_audio_before_resampler_zero_padding() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(100, 48000), PcmStatus::Ready);
        assert_eq!(pcm.write(100, &[12000; 960]), PcmStatus::Ready);
        assert_eq!(pcm.drain(100), PcmStatus::Busy);
        let samples: Vec<i16> = (0..5).flat_map(|_| render(&pcm)).collect();
        let jump = samples
            .windows(2)
            .map(|w| (i32::from(w[1]) - i32::from(w[0])).abs())
            .max()
            .unwrap();
        assert!(jump <= 100, "finish must taper before zero padding: {jump}");
        assert_eq!(samples[0], 0);
        assert_eq!(samples[959], 0);
        assert!(samples[960..].iter().all(|s| *s == 0));
    }
    #[test]
    fn starvation_rebuffers_and_finish_releases_short_tail() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(4, 48000), PcmStatus::Ready);
        for _ in 0..3 {
            assert_eq!(pcm.write(4, &[12000; 960]), PcmStatus::Ready);
        }
        for _ in 0..6 {
            let _ = render(&pcm);
        }
        assert!(render(&pcm).iter().all(|s| *s == 0));
        assert_eq!(pcm.write(4, &[12000; 480]), PcmStatus::Ready);
        for _ in 0..3 {
            assert!(render(&pcm).iter().all(|s| *s == 0));
        }
        assert_eq!(pcm.drain(4), PcmStatus::Busy);
        assert_eq!(render(&pcm)[0], 0);
        for _ in 0..2 {
            let _ = render(&pcm);
        }
        assert!(render(&pcm).iter().all(|s| *s == 0));
        assert_eq!(pcm.cancel(4), PcmStatus::Ready);
        assert_eq!(pcm.drain(4), PcmStatus::Stale);
    }
    #[test]
    fn queue_backpressure_does_not_consume_rejected_samples() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(5, 48000), PcmStatus::Ready);
        for _ in 0..10 {
            assert_eq!(pcm.write(5, &[1000; 960]), PcmStatus::Ready);
        }
        assert_eq!(pcm.write(5, &[2000; 480]), PcmStatus::Busy);
        assert_eq!(render(&pcm)[479], 1000);
        assert_eq!(pcm.write(5, &[2000; 480]), PcmStatus::Ready);
        for _ in 0..19 {
            assert!(render(&pcm).iter().all(|sample| *sample == 1000));
        }
        let tail = render(&pcm);
        assert_eq!(tail[0], 2000);
        assert_eq!(tail[479], 0);
    }
    #[test]
    fn drain_stays_busy_after_capacity_release_before_frame_submission() {
        unsafe extern "C" {
            fn nanocodex_pcm_test_render_completion(handle: *mut c_void) -> i32;
        }
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(201, 48000), PcmStatus::Ready);
        assert_eq!(pcm.write(201, &[12000; 480]), PcmStatus::Ready);
        assert_eq!(pcm.drain(201), PcmStatus::Busy);
        render(&pcm);
        render(&pcm);
        // Expire the prior block deadline, then probe inside the final callback
        // after its ring slots are freed but before its device frame is copied.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            unsafe { nanocodex_pcm_test_render_completion(pcm.handle) },
            1
        );
        assert_eq!(unsafe { nanocodex_pcm_status(pcm.handle, 201) }, 1);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(unsafe { nanocodex_pcm_status(pcm.handle, 201) }, 0);
    }
    #[test]
    fn drain_backpressure_preserves_partial_tail_until_retry() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(200, 48000), PcmStatus::Ready);
        for _ in 0..10 {
            assert_eq!(pcm.write(200, &[12000; 960]), PcmStatus::Ready);
        }
        assert_eq!(pcm.write(200, &[12000; 120]), PcmStatus::Ready);
        assert_eq!(pcm.drain(200), PcmStatus::Busy);
        assert_eq!(pcm.pending, vec![12000; 120]);
        let mut samples = Vec::new();
        samples.extend(render(&pcm));
        assert_eq!(pcm.drain(200), PcmStatus::Busy);
        assert_eq!(pcm.pending, vec![12000; 120]);
        samples.extend(render(&pcm));
        assert_eq!(pcm.drain(200), PcmStatus::Busy);
        assert!(pcm.pending.is_empty());
        for _ in 0..20 {
            samples.extend(render(&pcm));
        }
        assert_eq!(samples[9719], 0);
        assert!(samples[9720..].iter().all(|s| *s == 0));
        assert!(
            samples
                .windows(2)
                .all(|w| (i32::from(w[1]) - i32::from(w[0])).abs() <= 50)
        );
    }
    #[test]
    fn resamples_split_24k_chunks_and_drains_partial_tail() {
        let mut pcm = Pcm::new();
        let _factory = libwebrtc::peer_connection_factory::PeerConnectionFactory::default();
        assert_eq!(pcm.begin(10, 24000), PcmStatus::Ready);
        for _ in 0..3 {
            assert_eq!(pcm.write(10, &[10000; 113]), PcmStatus::Ready);
        }
        assert!(render(&pcm).iter().all(|sample| *sample == 0));
        assert_eq!(pcm.drain(10), PcmStatus::Busy);
        assert!(render(&pcm).iter().any(|sample| sample.abs() > 9000));
        assert_eq!(pcm.write(10, &[10000; 100]), PcmStatus::Stale);
        let _ = render(&pcm);
        let _ = render(&pcm);
        std::thread::sleep(Duration::from_millis(15));
        assert_eq!(pcm.drain(10), PcmStatus::Busy);
        std::thread::sleep(Duration::from_millis(105));
        assert_eq!(pcm.drain(10), PcmStatus::Ready);
    }
}
