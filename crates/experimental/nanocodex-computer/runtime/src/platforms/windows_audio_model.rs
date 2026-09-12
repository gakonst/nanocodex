//! Bounded computer-loopback recorder and PCM conversion, independent of COM.
//! Providers are created, polled, stopped and destroyed on one owning thread.
use crate::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const RATE: u32 = 24_000;
pub const MAX_DURATION_MS: u64 = 300_000;
pub const MAX_FRAMES: usize = RATE as usize * 300;
pub const MAX_PACKET_BYTES: usize = 4 * 1024 * 1024;
pub const SILENT: u32 = 2;
pub const DISCONTINUITY: u32 = 1;
pub const TIMESTAMP_ERROR: u32 = 4;
const TAPS: usize = 64;
const HALF: i64 = TAPS as i64 / 2;
const RUN: u8 = 0;
const STOP: u8 = 1;
const CANCEL: u8 = 2;

pub fn recording_duration(args: &Value) -> Result<u64> {
    let duration = args
        .get("max_duration_ms")
        .or_else(|| args.get("maxDurationMilliseconds"))
        .filter(|v| !v.is_null())
        .map(|v| {
            v.as_u64().ok_or_else(|| {
                Error::invalid(
                    "audio recording duration must be an integer from 100 through 300000",
                )
            })
        })
        .transpose()?
        .unwrap_or(60_000);
    if !(100..=MAX_DURATION_MS).contains(&duration) {
        return Err(Error::invalid(
            "audio recording duration must be an integer from 100 through 300000",
        ));
    }
    Ok(duration)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Pcm,
    Float,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    pub valid_bits: u16,
    pub block_align: u16,
    pub channel_mask: u32,
    pub encoding: Encoding,
}
impl Format {
    pub fn validate(self) -> Result<Self> {
        if !(8_000..=192_000).contains(&self.rate)
            || !(1..=18).contains(&self.channels)
            || !match self.encoding {
                Encoding::Pcm => {
                    matches!(self.bits, 8 | 16 | 24 | 32)
                        && (1..=self.bits).contains(&self.valid_bits)
                        && (self.bits != 8 || self.valid_bits == 8)
                }
                Encoding::Float => matches!(self.bits, 32 | 64) && self.valid_bits == self.bits,
            }
            || self.block_align != self.channels * (self.bits / 8)
            || (self.channel_mask == 0 && self.channels > 2)
            || (self.channel_mask != 0
                && (self.channel_mask.count_ones() != self.channels as u32
                    || self.channel_mask & !0x3ffff != 0))
        {
            return Err(Error::action(
                "Unsupported or malformed WASAPI PCM format/channel layout",
            ));
        }
        Ok(self)
    }
    pub fn packet_bytes(self, frames: u32) -> Result<usize> {
        self.validate()?;
        let bytes = (frames as usize)
            .checked_mul(self.block_align as usize)
            .filter(|bytes| *bytes <= MAX_PACKET_BYTES)
            .ok_or_else(|| Error::action("WASAPI packet exceeds byte bound"))?;
        if frames > self.rate {
            return Err(Error::action(
                "WASAPI packet exceeds one second of source frames",
            ));
        }
        Ok(bytes)
    }
    fn weights(self) -> Vec<[f64; 2]> {
        if self.channels == 1 {
            return vec![[1., 1.]];
        }
        if self.channel_mask == 0 {
            return vec![[1., 0.], [0., 1.]];
        }
        // WAVEFORMATEXTENSIBLE channel order is ascending speaker-mask bit order.
        let surround = std::f64::consts::FRAC_1_SQRT_2;
        let mut weights: Vec<_> = (0..18)
            .filter(|bit| self.channel_mask & (1 << bit) != 0)
            .map(|bit| match bit {
                0 => [1., 0.],
                1 => [0., 1.],
                2 => [surround, surround],
                3 | 8 | 11 | 13 | 16 => [0.5, 0.5],
                4 | 6 | 9 => [surround, 0.],
                5 | 7 | 10 => [0., surround],
                12 | 15 => [0.5, 0.],
                14 | 17 => [0., 0.5],
                _ => unreachable!(),
            })
            .collect();
        let sums = [
            weights.iter().map(|v| v[0]).sum::<f64>().max(1.),
            weights.iter().map(|v| v[1]).sum::<f64>().max(1.),
        ];
        for w in &mut weights {
            w[0] /= sums[0];
            w[1] /= sums[1];
        }
        weights
    }
    fn sample(self, data: &[u8]) -> Result<f64> {
        let value = match (self.encoding, self.bits) {
            (Encoding::Float, 32) => f32::from_le_bytes(data.try_into().unwrap()) as f64,
            (Encoding::Float, 64) => f64::from_le_bytes(data.try_into().unwrap()),
            (Encoding::Pcm, 8) => (data[0] as f64 - 128.) / 128.,
            (Encoding::Pcm, bits) => {
                let raw = match bits {
                    16 => i16::from_le_bytes(data.try_into().unwrap()) as i32,
                    24 => i32::from_le_bytes([0, data[0], data[1], data[2]]) >> 8,
                    32 => i32::from_le_bytes(data.try_into().unwrap()),
                    _ => unreachable!(),
                };
                let padding = self.bits - self.valid_bits;
                if padding != 0 && raw as u32 & ((1u32 << padding) - 1) != 0 {
                    return Err(Error::action("WASAPI PCM valid-bit padding is nonzero"));
                }
                (raw >> padding) as f64 / (1u64 << (self.valid_bits - 1)) as f64
            }
            _ => unreachable!(),
        };
        if !value.is_finite() {
            return Err(Error::action("Nonfinite WASAPI PCM sample"));
        }
        Ok(value.clamp(-1., 1.))
    }
}

#[derive(Debug)]
pub struct Packet {
    /// Owned copy only. Native GetBuffer has already been released.
    pub data: Vec<u8>,
    pub frames: u32,
    pub flags: u32,
    pub position: u64,
}

/// Parse a bounded WAVEFORMATEX/EXTENSIBLE byte record without packed references.
pub fn parse_wave_format(bytes: &[u8]) -> Result<Format> {
    if bytes.len() < 18 || bytes.len() > 4096 {
        return Err(Error::action("Invalid WASAPI format record size"));
    }
    let word = |offset| u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
    let dword = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    if bytes.len() != 18 + word(16) as usize {
        return Err(Error::action("Truncated WASAPI format extension"));
    }
    let (encoding, valid_bits, mask) = match word(0) {
        1 | 3 if word(16) == 0 => (
            if word(0) == 1 {
                Encoding::Pcm
            } else {
                Encoding::Float
            },
            word(14),
            0,
        ),
        0xfffe if word(16) >= 22 => {
            if bytes[28..40] != [0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71] {
                return Err(Error::action(
                    "Unsupported WASAPI extensible subformat GUID",
                ));
            }
            let encoding = match dword(24) {
                1 => Encoding::Pcm,
                3 => Encoding::Float,
                _ => return Err(Error::action("Unsupported WASAPI extensible PCM encoding")),
            };
            (encoding, word(18), dword(20))
        }
        _ => return Err(Error::action("Unsupported WASAPI WAVEFORMAT tag/extension")),
    };
    let format = Format {
        rate: dword(4),
        channels: word(2),
        bits: word(14),
        valid_bits,
        block_align: word(12),
        channel_mask: mask,
        encoding,
    }
    .validate()?;
    if dword(8) != format.rate * format.block_align as u32 {
        return Err(Error::action("Invalid WASAPI average byte rate"));
    }
    Ok(format)
}

/// Exactly one same-thread release even when validation/copying fails or unwinds.
/// Ordinary release errors are returned, never converted into packet success.
pub struct PacketLease<F: FnOnce(u32) -> Result<()>> {
    frames: u32,
    release: Option<F>,
}
impl<F: FnOnce(u32) -> Result<()>> PacketLease<F> {
    pub fn new(frames: u32, release: F) -> Self {
        Self {
            frames,
            release: Some(release),
        }
    }
    pub fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        let consumed = if result.is_ok() { self.frames } else { 0 };
        let cleanup = self.release.take().unwrap()(consumed);
        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(mut error), Err(cleanup)) => {
                error
                    .message
                    .push_str(&format!("; ReleaseBuffer failed: {}", cleanup.message));
                Err(error)
            }
        }
    }
}
impl<F: FnOnce(u32) -> Result<()>> Drop for PacketLease<F> {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release(0);
        }
    }
}

/// Streaming Blackman-windowed sinc resampler. Exact rational phases, 64 taps,
/// normalized DC gain and a 90%-Nyquist cutoff before downsampling.
/// Equal-rate input bypasses the filter. No source rate is merely relabeled.
pub struct Converter {
    format: Format,
    weights: Vec<[f64; 2]>,
    kernels: Vec<[f64; TAPS]>,
    phase_step: u32,
    queue: VecDeque<[f64; 2]>,
    base: i64,
    source_frames: u64,
    output: Vec<i16>,
    limit: usize,
    position: Option<u64>,
    packets: u64,
    timestamp_errors: u64,
}
impl Converter {
    pub fn new(format: Format, limit: usize) -> Result<Self> {
        let format = format.validate()?;
        if limit == 0 || limit > MAX_FRAMES {
            return Err(Error::invalid("Invalid WASAPI output frame bound"));
        }
        let mut a = format.rate;
        let mut b = RATE;
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let phases = if format.rate == RATE { 0 } else { RATE / a };
        let cutoff = 0.45 * (RATE as f64 / format.rate as f64).min(1.);
        let mut kernels = Vec::new();
        kernels
            .try_reserve_exact(phases as usize)
            .map_err(|_| Error::action("WASAPI resampler allocation failed"))?;
        for phase in 0..phases {
            let fraction = phase as f64 / phases as f64;
            let mut kernel = [0.; TAPS];
            for (tap, weight) in kernel.iter_mut().enumerate() {
                let x = tap as f64 - (HALF - 1) as f64 - fraction;
                let z = 2. * cutoff * x;
                let sinc = if z.abs() < 1e-12 {
                    1.
                } else {
                    (std::f64::consts::PI * z).sin() / (std::f64::consts::PI * z)
                };
                let window = 0.42
                    + 0.5 * (std::f64::consts::PI * x / HALF as f64).cos()
                    + 0.08 * (2. * std::f64::consts::PI * x / HALF as f64).cos();
                *weight = 2. * cutoff * sinc * window;
            }
            let sum: f64 = kernel.iter().sum();
            for weight in &mut kernel {
                *weight /= sum;
            }
            kernels.push(kernel);
        }
        Ok(Self {
            format,
            weights: format.weights(),
            kernels,
            phase_step: a,
            queue: VecDeque::new(),
            base: 0,
            source_frames: 0,
            output: vec![],
            limit,
            position: None,
            packets: 0,
            timestamp_errors: 0,
        })
    }
    pub fn frames(&self) -> usize {
        self.output.len() / 2
    }
    pub fn full(&self) -> bool {
        self.source_frames >= (self.limit as u64 * self.format.rate as u64).div_ceil(RATE as u64)
    }
    pub fn push(&mut self, packet: Packet) -> Result<()> {
        let bytes = self.format.packet_bytes(packet.frames)?;
        if packet.frames == 0
            || packet.flags & !(SILENT | DISCONTINUITY | TIMESTAMP_ERROR) != 0
            || if packet.flags & SILENT == 0 {
                packet.data.len() != bytes
            } else {
                !packet.data.is_empty()
            }
        {
            return Err(Error::action("Malformed WASAPI packet/flags"));
        }
        // Windows may mark the very first packet discontinuous. Later loss must
        // never be quietly concatenated into apparently continuous audio.
        if self.packets != 0 && packet.flags & DISCONTINUITY != 0 {
            return Err(Error::action(
                "WASAPI audio data discontinuity; recording is incomplete",
            ));
        }
        if packet.flags & TIMESTAMP_ERROR != 0 {
            self.timestamp_errors += 1;
            self.position = None;
        } else {
            if self
                .position
                .is_some_and(|expected| expected != packet.position)
            {
                return Err(Error::action(
                    "WASAPI device frame position is discontinuous",
                ));
            }
            self.position = Some(
                packet
                    .position
                    .checked_add(packet.frames as u64)
                    .ok_or_else(|| Error::action("WASAPI frame position overflow"))?,
            );
        }
        self.packets += 1;
        let source_limit = (self.limit as u64 * self.format.rate as u64).div_ceil(RATE as u64);
        let accepted =
            (packet.frames as u64).min(source_limit.saturating_sub(self.source_frames)) as usize;
        self.queue
            .try_reserve(accepted)
            .map_err(|_| Error::action("WASAPI PCM queue allocation failed"))?;
        for frame in 0..accepted {
            let mut pair = [0.; 2];
            if packet.flags & SILENT == 0 {
                let frame = &packet.data[frame * self.format.block_align as usize
                    ..(frame + 1) * self.format.block_align as usize];
                for (sample, weight) in frame
                    .chunks_exact((self.format.bits / 8) as usize)
                    .zip(&self.weights)
                {
                    let value = self.format.sample(sample)?;
                    pair[0] += value * weight[0];
                    pair[1] += value * weight[1];
                }
            }
            self.queue.push_back(pair);
        }
        self.source_frames += accepted as u64;
        self.produce(false)?;
        Ok(())
    }
    fn produce(&mut self, final_packet: bool) -> Result<()> {
        let count = (self.source_frames * RATE as u64 / self.format.rate as u64)
            .min(self.limit as u64) as usize;
        while self.frames() < count {
            let numerator = self.frames() as u64 * self.format.rate as u64;
            let center = (numerator / RATE as u64) as i64;
            if !final_packet
                && self.format.rate != RATE
                && center + HALF >= self.source_frames as i64
            {
                break;
            }
            let mut pair = [0.; 2];
            if self.format.rate == RATE {
                pair = self.queue[(center - self.base) as usize];
            } else {
                let phase = (numerator % RATE as u64) as usize / self.phase_step as usize;
                for (tap, weight) in self.kernels[phase].iter().enumerate() {
                    let index = center + tap as i64 - (HALF - 1);
                    if index >= 0 && index < self.source_frames as i64 {
                        let sample = self.queue[(index - self.base) as usize];
                        pair[0] += sample[0] * weight;
                        pair[1] += sample[1] * weight;
                    }
                }
            }
            if self.output.capacity() - self.output.len() < 2 {
                self.output
                    .try_reserve((self.limit * 2 - self.output.len()).min(8192))
                    .map_err(|_| Error::action("WASAPI output allocation failed"))?;
            }
            self.output.extend(pair.map(|value| {
                (value.clamp(-1., 1.) * 32768.)
                    .round()
                    .clamp(-32768., 32767.) as i16
            }));
            let next = (self.frames() as u64 * self.format.rate as u64 / RATE as u64) as i64;
            let retain = if self.format.rate == RATE {
                next
            } else {
                (next - (HALF - 1)).max(0)
            };
            while self.base < retain && !self.queue.is_empty() {
                self.queue.pop_front();
                self.base += 1;
            }
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Output> {
        self.produce(true)?;
        if self.output.is_empty() {
            return Err(Error::action("Audio recording produced no samples"));
        }
        Ok(Output {
            samples: self.output,
            source_format: self.format,
            timestamp_errors: self.timestamp_errors,
        })
    }
}

#[derive(Debug)]
pub struct Output {
    pub samples: Vec<i16>,
    pub source_format: Format,
    pub timestamp_errors: u64,
}
pub fn wav(samples: &[i16]) -> Result<Vec<u8>> {
    if samples.is_empty() || !samples.len().is_multiple_of(2) || samples.len() / 2 > MAX_FRAMES {
        return Err(Error::invalid("Invalid WASAPI stereo output size"));
    }
    let size = (samples.len() * 2) as u32;
    let mut out = Vec::new();
    out.try_reserve_exact(44 + size as usize)
        .map_err(|_| Error::action("WASAPI WAV allocation failed"))?;
    out.extend(b"RIFF");
    out.extend((36 + size).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 4).to_le_bytes());
    out.extend(4u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(size.to_le_bytes());
    for sample in samples {
        out.extend(sample.to_le_bytes());
    }
    Ok(out)
}

/// A successful packet must already be copied and released before this boundary.
/// Deliberately not Send: COM providers are constructed on the recording thread.
pub trait Provider {
    fn format(&self) -> Format;
    /// Begin only after format/conversion setup has finished and cancellation was checked.
    fn start(&mut self, control: &Control) -> Result<()>;
    fn next_packet(&mut self) -> Result<Option<Packet>>;
    fn wait(&mut self, timeout: Duration) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
}
#[derive(Clone)]
pub struct Control {
    signal: Arc<AtomicU8>,
    startup_deadline: Instant,
}
impl Control {
    pub fn check_start(&self) -> Result<()> {
        if self.signal.load(Ordering::SeqCst) != RUN || Instant::now() >= self.startup_deadline {
            Err(Error::new(-32008, "WASAPI startup cancelled or timed out"))
        } else {
            Ok(())
        }
    }
    pub fn cancelled(&self) -> bool {
        self.signal.load(Ordering::SeqCst) == CANCEL
    }
}
type Factory = dyn Fn(&Control) -> Result<Box<dyn Provider>> + Send + Sync;
#[derive(Default)]
struct State {
    finished: bool,
    frames: usize,
    result: Option<Result<Output>>,
    error: Option<Error>,
}
struct Recording {
    owner: String,
    signal: Arc<AtomicU8>,
    state: Arc<Mutex<State>>,
    done: mpsc::Receiver<()>,
    thread: Option<JoinHandle<()>>,
}
impl Drop for Recording {
    fn drop(&mut self) {
        self.signal.store(CANCEL, Ordering::SeqCst);
        if self.thread.as_ref().is_some_and(|t| t.is_finished()) {
            let _ = self.thread.take().unwrap().join();
        }
        // A wedged native call cannot be killed safely in-process. Detach, keep
        // its cancellation flag alive, suppress output, and disable owner reuse.
    }
}
pub struct Audio {
    factory: Arc<Factory>,
    active: Option<Recording>,
    timeout: Duration,
    disabled: bool,
}
impl Audio {
    pub fn new(
        factory: impl Fn(&Control) -> Result<Box<dyn Provider>> + Send + Sync + 'static,
    ) -> Self {
        Self::with_timeout(factory, Duration::from_secs(5))
    }
    pub fn with_timeout(
        factory: impl Fn(&Control) -> Result<Box<dyn Provider>> + Send + Sync + 'static,
        timeout: Duration,
    ) -> Self {
        Self {
            factory: Arc::new(factory),
            active: None,
            timeout,
            disabled: false,
        }
    }
    fn check_owner(&self, owner: &str) -> Result<()> {
        if owner.is_empty() {
            return Err(Error::invalid("Audio owner is required"));
        }
        if self.active.as_ref().is_some_and(|r| r.owner != owner) {
            return Err(Error::new(
                -32001,
                "Audio recording belongs to another owner",
            ));
        }
        Ok(())
    }
    pub fn execute(&mut self, method: &str, owner: &str, args: &Value) -> Result<Value> {
        self.check_owner(owner)?;
        match method {
            "start" | "start_audio_recording" => self.start(owner, args),
            "stop" | "stop_audio_recording" => self.stop(owner),
            "status" => self.status(),
            _ => Err(Error::unsupported("Unknown native audio method")),
        }
    }
    fn status(&self) -> Result<Value> {
        match &self.active {
            None => Ok(json!({"active":false})),
            Some(recording) => {
                let state = recording
                    .state
                    .lock()
                    .map_err(|_| Error::action("WASAPI state poisoned"))?;
                Ok(
                    json!({"active":!state.finished,"completed":state.finished,"scope":"system","pid":null,
                    "frames":state.frames,"error":state.error.as_ref().map(|e| &e.message)}),
                )
            }
        }
    }
    fn start(&mut self, owner: &str, args: &Value) -> Result<Value> {
        if self.disabled {
            return Err(Error::new(
                -32008,
                "WASAPI recorder disabled after native timeout",
            ));
        }
        if self.active.is_some() {
            return Err(Error::action(
                "Audio recording already exists; stop it before starting another",
            ));
        }
        if args.get("scope").is_some_and(|v| v != "system") || args.get("pid").is_some() {
            return Err(Error::invalid(
                "Windows computer loopback requires system scope without pid",
            ));
        }
        let duration = recording_duration(args)?;
        let signal = Arc::new(AtomicU8::new(RUN));
        let state = Arc::new(Mutex::new(State::default()));
        let control = Control {
            signal: signal.clone(),
            startup_deadline: Instant::now() + self.timeout,
        };
        let worker_state = state.clone();
        let factory = self.factory.clone();
        let (ready, started) = mpsc::sync_channel(1);
        let (done, finished) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("skyre-wasapi".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut provider = factory(&control)?;
                    let startup = control
                        .check_start()
                        .and_then(|_| {
                            Converter::new(
                                provider.format(),
                                (duration * RATE as u64 / 1000) as usize,
                            )
                        })
                        .and_then(|converter| {
                            control.check_start()?;
                            provider.start(&control)?;
                            control.check_start()?;
                            Ok(converter)
                        });
                    let collected = match startup {
                        Err(error) => Err(error),
                        Ok(mut converter) => {
                            let _ = ready.try_send(Ok(()));
                            record(
                                &mut *provider,
                                &control,
                                &mut converter,
                                &worker_state,
                                Duration::from_millis(duration),
                            )
                            .map(|_| converter)
                        }
                    };
                    let cleanup = provider.stop();
                    let mut converter = match (collected, cleanup) {
                        (Ok(converter), Ok(())) => Ok(converter),
                        (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
                        (Err(mut error), Err(cleanup)) => {
                            error
                                .message
                                .push_str(&format!("; cleanup failed: {}", cleanup.message));
                            Err(error)
                        }
                    }?;
                    if control.cancelled() {
                        return Ok(Output {
                            samples: vec![],
                            source_format: provider.format(),
                            timestamp_errors: 0,
                        });
                    }
                    // Stop freezes the native queue. Consume its remaining packets,
                    // bounded independently, then flush only the FIR's finite tail.
                    let drain_deadline = Instant::now() + Duration::from_secs(1);
                    for count in 0..=128 {
                        if converter.full() || control.cancelled() {
                            break;
                        }
                        if count == 128 || Instant::now() >= drain_deadline {
                            return Err(Error::new(
                                -32008,
                                "WASAPI final packet drain exceeded bound",
                            ));
                        }
                        match provider.next_packet()? {
                            Some(packet) => converter.push(packet)?,
                            None => break,
                        }
                    }
                    converter.finish()
                }))
                .unwrap_or_else(|_| Err(Error::action("WASAPI controller panicked")));
                if let Err(error) = &result {
                    let _ = ready.try_send(Err(error.clone()));
                }
                if let Ok(mut state) = worker_state.lock() {
                    state.finished = true;
                    state.error = result.as_ref().err().cloned();
                    if let Ok(output) = &result {
                        state.frames = output.samples.len() / 2;
                    }
                    if !control.cancelled() {
                        state.result = Some(result);
                    }
                }
                let _ = done.try_send(());
            })?;
        let recording = Recording {
            owner: owner.into(),
            signal,
            state,
            done: finished,
            thread: Some(worker),
        };
        match started.recv_timeout(self.timeout) {
            Ok(Ok(())) => {
                self.active = Some(recording);
                self.status()
            }
            Ok(Err(error)) => {
                if error.code == -32008 {
                    self.disabled = true;
                }
                Err(error)
            }
            Err(_) => {
                self.disabled = true;
                Err(Error::new(-32008, "WASAPI startup timed out"))
            }
        }
    }
    fn finish_recording(&mut self, cancel: bool) -> Result<Recording> {
        let mut recording = self
            .active
            .take()
            .ok_or_else(|| Error::action("No recording exists"))?;
        recording
            .signal
            .store(if cancel { CANCEL } else { STOP }, Ordering::SeqCst);
        if recording.done.recv_timeout(self.timeout).is_err() {
            self.disabled = true;
            return Err(Error::new(
                -32008,
                "WASAPI stop/teardown timed out; recorder disabled",
            ));
        }
        if let Some(worker) = recording.thread.take() {
            worker
                .join()
                .map_err(|_| Error::action("WASAPI controller panicked"))?;
        }
        let state = recording
            .state
            .lock()
            .map_err(|_| Error::action("WASAPI state poisoned"))?;
        if state.error.as_ref().is_some_and(|e| e.code == -32008) {
            self.disabled = true;
        }
        drop(state);
        Ok(recording)
    }
    fn stop(&mut self, owner: &str) -> Result<Value> {
        self.check_owner(owner)?;
        let recording = self.finish_recording(false)?;
        let result = recording
            .state
            .lock()
            .map_err(|_| Error::action("WASAPI state poisoned"))?
            .result
            .take()
            .ok_or_else(|| Error::action("WASAPI output was cancelled or already consumed"))??;
        let frames = result.samples.len() / 2;
        let bytes = wav(&result.samples)?;
        Ok(
            json!({"mime_type":"audio/wav","data":STANDARD.encode(bytes),"sample_rate":RATE,"channels":2,
            "frames":frames,"duration_ms":frames as u64 * 1000 / RATE as u64,
            "source_sample_rate":result.source_format.rate,"source_channels":result.source_format.channels,
            "timestamp_errors":result.timestamp_errors}),
        )
    }
    pub fn end_session(&mut self, owner: &str) -> Result<()> {
        self.check_owner(owner)?;
        if self.active.is_some() {
            let recording = self.finish_recording(true)?;
            let state = recording
                .state
                .lock()
                .map_err(|_| Error::action("WASAPI state poisoned"))?;
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
        }
        Ok(())
    }
}
fn record(
    provider: &mut dyn Provider,
    control: &Control,
    converter: &mut Converter,
    state: &Mutex<State>,
    duration: Duration,
) -> Result<()> {
    let deadline = Instant::now() + duration;
    loop {
        if control.signal.load(Ordering::SeqCst) != RUN
            || Instant::now() >= deadline
            || converter.full()
        {
            return Ok(());
        }
        // Bound a malicious/overproducing provider and recheck time/cancellation
        // between every packet; native buffers are released before conversion.
        let mut drained = false;
        for _ in 0..128 {
            if control.signal.load(Ordering::SeqCst) != RUN
                || Instant::now() >= deadline
                || converter.full()
            {
                return Ok(());
            }
            match provider.next_packet()? {
                None => {
                    drained = true;
                    break;
                }
                Some(packet) => converter.push(packet)?,
            }
            state
                .lock()
                .map_err(|_| Error::action("WASAPI state poisoned"))?
                .frames = converter.frames();
        }
        if !drained {
            return Err(Error::action(
                "WASAPI provider exceeded queued-packet bound",
            ));
        }
        provider.wait(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        )?;
    }
}
