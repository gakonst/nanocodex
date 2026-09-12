//! Explicit ScreenCaptureKit loopback audio. Application scope is the default;
//! system scope requires an explicit option. No microphone is captured.
use crate::{Error, Result};
use base64::Engine as _;
use block2::RcBlock;
use core_foundation::{
    base::CFRelease,
    runloop::{CFRunLoop, kCFRunLoopDefaultMode},
};
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_media::{
    CMAudioFormatDescriptionGetStreamBasicDescription, CMBlockBuffer, CMSampleBuffer,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput,
    SCStreamOutputType,
};
use serde_json::{Value, json};
use std::{
    mem, ptr,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const RATE: u32 = 24_000;
pub const CHANNELS: u16 = 2;
/// Installed embedded-Sky audio contract, validated before any capture API.
pub fn recording_duration(args: &Value) -> Result<u64> {
    let duration = args
        .get("max_duration_ms")
        .or_else(|| args.get("maxDurationMilliseconds"))
        .filter(|value| !value.is_null())
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                Error::invalid(
                    "audio recording duration must be an integer from 100 through 300000",
                )
            })
        })
        .transpose()?
        .unwrap_or(60_000);
    if !(100..=300_000).contains(&duration) {
        return Err(Error::invalid(
            "audio recording duration must be an integer from 100 through 300000",
        ));
    }
    Ok(duration)
}
#[derive(Clone, Copy, Debug)]
pub struct PcmFormat {
    pub channels: u32,
    pub bits: u32,
    pub float: bool,
    pub big_endian: bool,
    pub planar: bool,
    pub rate: u32,
}
/// Decode validated PCM into interleaved little-endian signed 16-bit stereo.
/// Planar channel order is preserved; mono is duplicated explicitly.
pub fn convert_pcm(
    buffers: &[(&[u8], u32)],
    format: PcmFormat,
    frame_limit: usize,
) -> Result<Vec<i16>> {
    if format.rate != RATE
        || !(1..=2).contains(&format.channels)
        || !matches!((format.float, format.bits), (true, 32) | (false, 16))
    {
        return Err(Error::action("Unsupported captured PCM format"));
    }
    let bytes = (format.bits / 8) as usize;
    let expected = if format.planar {
        format.channels as usize
    } else {
        1
    };
    if buffers.len() != expected
        || buffers
            .iter()
            .any(|(_, channels)| *channels != if format.planar { 1 } else { format.channels })
    {
        return Err(Error::action("Invalid audio channel layout"));
    }
    let stride = bytes
        * if format.planar {
            1
        } else {
            format.channels as usize
        };
    if buffers.iter().any(|(data, _)| data.len() % stride != 0) {
        return Err(Error::action("Truncated PCM frame"));
    }
    let frames = buffers[0].0.len() / stride;
    if buffers
        .iter()
        .any(|(data, _)| data.len() / stride != frames)
    {
        return Err(Error::action("Unequal planar PCM frame counts"));
    }
    let mut out = Vec::with_capacity(frames.min(frame_limit) * 2);
    for frame in 0..frames.min(frame_limit) {
        for channel in 0..2usize {
            let channel = channel.min(format.channels as usize - 1);
            let (buffer, offset) = if format.planar {
                (channel, frame * bytes)
            } else {
                (0, (frame * format.channels as usize + channel) * bytes)
            };
            let data = &buffers[buffer].0[offset..offset + bytes];
            let sample = if format.float {
                let value = if format.big_endian {
                    f32::from_be_bytes(data.try_into().unwrap())
                } else {
                    f32::from_le_bytes(data.try_into().unwrap())
                };
                if !value.is_finite() {
                    return Err(Error::action("Nonfinite PCM sample"));
                }
                (value.clamp(-1., 1.) * 32768.)
                    .round()
                    .clamp(-32768., 32767.) as i16
            } else if format.big_endian {
                i16::from_be_bytes(data.try_into().unwrap())
            } else {
                i16::from_le_bytes(data.try_into().unwrap())
            };
            out.push(sample);
        }
    }
    Ok(out)
}
pub fn wav(samples: &[i16]) -> Result<Vec<u8>> {
    if !samples.len().is_multiple_of(CHANNELS as usize) {
        return Err(Error::invalid("Incomplete stereo frame"));
    }
    let data_size: u32 = samples
        .len()
        .checked_mul(2)
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| Error::invalid("Audio output too large"))?;
    let riff_size = data_size
        .checked_add(36)
        .ok_or_else(|| Error::invalid("Audio output too large"))?;
    let mut out = Vec::with_capacity(44 + data_size as usize);
    out.extend(b"RIFF");
    out.extend(riff_size.to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(CHANNELS.to_le_bytes());
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * CHANNELS as u32 * 2).to_le_bytes());
    out.extend((CHANNELS * 2).to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_size.to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    Ok(out)
}
struct Collector {
    samples: Vec<i16>,
    limit: usize,
    error: Option<String>,
    finished: bool,
}
struct OutputState {
    collector: Arc<Mutex<Collector>>,
}
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SkyreAudioOutput"]
    #[ivars=OutputState]
    struct Output;
    unsafe impl NSObjectProtocol for Output {}
    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
                    let mut state = self
                        .ivars()
                        .collector
                        .lock()
                        .map_err(|_| Error::action("Audio collector poisoned"))?;
                    if state.finished || state.error.is_some() {
                        return Ok(());
                    }
                    let remaining = state.limit.saturating_sub(state.samples.len() / 2);
                    if remaining == 0 {
                        return Ok(());
                    }
                    let decoded = unsafe { decode_sample(sample, remaining) }?;
                    state.samples.extend(decoded);
                    Ok(())
                }));
            let error = match result {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error.message),
                Err(_) => Some("Audio callback panicked".into()),
            };
            if let Some(error) = error
                && let Ok(mut state) = self.ivars().collector.lock()
            {
                state.error = Some(error);
            }
        }
    }
);
impl Output {
    fn new(collector: Arc<Mutex<Collector>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(OutputState { collector });
        unsafe { msg_send![super(this), init] }
    }
}
unsafe fn decode_sample(sample: &CMSampleBuffer, limit: usize) -> Result<Vec<i16>> {
    unsafe {
        let description = sample
            .format_description()
            .ok_or_else(|| Error::action("Audio sample has no format"))?;
        let desc = CMAudioFormatDescriptionGetStreamBasicDescription(&description)
            .as_ref()
            .ok_or_else(|| Error::action("Audio format has no PCM description"))?;
        if desc.mFormatID != u32::from_be_bytes(*b"lpcm") || desc.mSampleRate != RATE as f64 {
            return Err(Error::action("Unexpected SCK audio format/rate"));
        }
        let flags = desc.mFormatFlags;
        if flags & 1 == 0 && flags & 4 == 0 {
            return Err(Error::action("Unsupported unsigned audio samples"));
        }
        let format = PcmFormat {
            channels: desc.mChannelsPerFrame,
            bits: desc.mBitsPerChannel,
            float: flags & 1 != 0,
            big_endian: flags & 2 != 0,
            planar: flags & 32 != 0,
            rate: RATE,
        };
        let expected_frame = (format.bits / 8) * if format.planar { 1 } else { format.channels };
        if desc.mBytesPerFrame != expected_frame {
            return Err(Error::action("Unsupported padded audio frames"));
        }
        let mut needed = 0;
        let _ = sample.audio_buffer_list_with_retained_block_buffer(
            &mut needed,
            ptr::null_mut(),
            0,
            None,
            None,
            0,
            ptr::null_mut(),
        );
        if needed < mem::size_of::<AudioBufferList>() || needed > 4096 {
            return Err(Error::action("Invalid audio buffer-list size"));
        }
        let mut storage = vec![0u64; needed.div_ceil(8)];
        let raw = storage.as_mut_ptr() as *mut AudioBufferList;
        let mut block: *mut CMBlockBuffer = ptr::null_mut();
        let code = sample.audio_buffer_list_with_retained_block_buffer(
            &mut needed,
            raw,
            storage.len() * 8,
            None,
            None,
            0,
            &mut block,
        );
        struct Block(*mut CMBlockBuffer);
        impl Drop for Block {
            fn drop(&mut self) {
                if !self.0.is_null() {
                    unsafe { CFRelease(self.0 as _) }
                }
            }
        }
        let _retained = Block(block);
        if code != 0 {
            return Err(Error::action(format!(
                "Audio buffer extraction failed ({code})"
            )));
        }
        let count = (*raw).mNumberBuffers as usize;
        if !(1..=2).contains(&count)
            || mem::offset_of!(AudioBufferList, mBuffers) + count * mem::size_of::<AudioBuffer>()
                > storage.len() * 8
        {
            return Err(Error::action("Invalid audio buffer count"));
        }
        let buffers =
            std::slice::from_raw_parts(ptr::addr_of!((*raw).mBuffers) as *const AudioBuffer, count);
        let mut views = Vec::new();
        for b in buffers {
            if b.mData.is_null() || b.mDataByteSize > 8 * 1024 * 1024 {
                return Err(Error::action("Invalid audio buffer storage"));
            }
            views.push((
                std::slice::from_raw_parts(b.mData as *const u8, b.mDataByteSize as usize),
                b.mNumberChannels,
            ));
        }
        let stride = (format.bits / 8) as usize
            * if format.planar {
                1
            } else {
                format.channels as usize
            };
        if stride == 0
            || sample.num_samples() < 0
            || views[0].0.len() / stride != sample.num_samples() as usize
        {
            return Err(Error::action(
                "Audio sample count does not match its buffers",
            ));
        }
        convert_pcm(&views, format, limit)
    }
}
// SCK's asynchronous stream methods can be called from a queue other than its
// completion queue. Ownership is transferred once to the audio controller thread;
// only that thread invokes start/stop/removeOutput. Callback data uses a Mutex.
struct Built {
    stream: Retained<SCStream>,
    output: Retained<Output>,
}
unsafe impl Send for Built {}
impl Drop for Built {
    fn drop(&mut self) {
        // Covers failed starts and callbacks delivered after the waiting owner
        // timed out. Explicit stop still waits for its completion before returning.
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
            let _ = self.stream.removeStreamOutput_type_error(
                ProtocolObject::from_ref(&*self.output),
                SCStreamOutputType::Audio,
            );
        }
    }
}

fn pump(duration: Duration) {
    let started = Instant::now();
    unsafe { CFRunLoop::run_in_mode(kCFRunLoopDefaultMode, duration, true) };
    // A run loop with no registered source returns immediately. Preserve the
    // requested poll interval instead of busy-spinning through timeout windows.
    if let Some(remaining) = duration.checked_sub(started.elapsed()) {
        std::thread::sleep(remaining);
    }
}
fn receive<T>(rx: &mpsc::Receiver<T>, limit: Duration) -> Result<T> {
    let start = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(value) => return Ok(value),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(Error::action("Audio callback disconnected"));
            }
            _ => (),
        };
        if start.elapsed() >= limit {
            return Err(Error::action("Audio callback timed out"));
        }
        pump(Duration::from_millis(5));
    }
}
fn build(pid: Option<i32>, collector: Arc<Mutex<Collector>>) -> Result<Built> {
    let (send, recv) = mpsc::channel();
    let callback = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = (|| unsafe {
                if let Some(error) = error.as_ref() {
                    return Err(Error::action(error.localizedDescription().to_string()));
                }
                let content = content
                    .as_ref()
                    .ok_or_else(|| Error::action("No shareable audio content"))?;
                let display = content
                    .displays()
                    .firstObject()
                    .ok_or_else(|| Error::action("No display for audio filter"))?;
                let windows = NSArray::new();
                let filter = if let Some(pid) = pid {
                    let app = content
                        .applications()
                        .iter()
                        .find(|a| a.processID() == pid)
                        .ok_or_else(|| Error::action("Audio target application not shareable"))?;
                    let apps = NSArray::from_retained_slice(&[app]);
                    SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
                        SCContentFilter::alloc(),
                        &display,
                        &apps,
                        &windows,
                    )
                } else {
                    let excluded = NSArray::new();
                    SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                        SCContentFilter::alloc(),
                        &display,
                        &excluded,
                        &windows,
                    )
                };
                let config = SCStreamConfiguration::new();
                config.setCapturesAudio(true);
                config.setSampleRate(RATE as isize);
                config.setChannelCount(CHANNELS as isize);
                config.setExcludesCurrentProcessAudio(false);
                config.setWidth(2);
                config.setHeight(2);
                config.setShowsCursor(false);
                let stream = SCStream::initWithFilter_configuration_delegate(
                    SCStream::alloc(),
                    &filter,
                    &config,
                    None,
                );
                let output = Output::new(collector.clone());
                stream
                    .addStreamOutput_type_sampleHandlerQueue_error(
                        ProtocolObject::from_ref(&*output),
                        SCStreamOutputType::Audio,
                        None,
                    )
                    .map_err(|e| Error::action(e.localizedDescription().to_string()))?;
                Ok(Built { stream, output })
            })();
            let _ = send.send(result);
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true,false,&callback)
    };
    receive(&recv, Duration::from_secs(10))?
}
fn completion(operation: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>)) -> Result<()> {
    let (send, recv) = mpsc::channel();
    let callback = RcBlock::new(move |error: *mut NSError| {
        let result = unsafe { error.as_ref() }.map_or(Ok(()), |e| {
            Err(Error::action(e.localizedDescription().to_string()))
        });
        let _ = send.send(result);
    });
    operation(&callback);
    receive(&recv, Duration::from_secs(10))?
}
struct Recording {
    owner: String,
    pid: Option<i32>,
    collector: Arc<Mutex<Collector>>,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Recording {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[derive(Default)]
pub struct Audio {
    active: Option<Recording>,
}
impl Audio {
    pub fn execute(&mut self, method: &str, owner: &str, args: &Value) -> Result<Value> {
        if owner.is_empty() {
            return Err(Error::invalid("Audio owner is required"));
        }
        match method {
            "start" | "start_audio_recording" => self.start(owner, args),
            "stop" | "stop_audio_recording" => self.stop(owner),
            "status" => {
                self.check_owner(owner)?;
                Ok(self.status())
            }
            _ => Err(Error::unsupported("Unknown native audio method")),
        }
    }
    fn check_owner(&self, owner: &str) -> Result<()> {
        if self.active.as_ref().is_some_and(|r| r.owner != owner) {
            Err(Error::new(
                -32001,
                "Audio recording belongs to another owner",
            ))
        } else {
            Ok(())
        }
    }
    fn status(&self) -> Value {
        match &self.active {
            None => json!({"active":false}),
            Some(r) => match r.collector.lock() {
                Ok(s) => {
                    json!({"active":!s.finished,"completed":s.finished,"pid":r.pid,"scope":if r.pid.is_some(){"application"}else{"system"},"frames":s.samples.len()/2,"error":s.error})
                }
                Err(_) => json!({"active":false,"error":"Audio collector poisoned"}),
            },
        }
    }
    fn start(&mut self, owner: &str, args: &Value) -> Result<Value> {
        if self.active.is_some() {
            return Err(Error::action(
                "Audio recording already exists; stop it before starting another",
            ));
        }
        let scope = args
            .get("scope")
            .map(|v| {
                v.as_str()
                    .ok_or_else(|| Error::invalid("Audio scope must be a string"))
            })
            .transpose()?
            .unwrap_or("application");
        let pid = match scope {
            "application" => Some(
                args["pid"]
                    .as_i64()
                    .and_then(|p| i32::try_from(p).ok())
                    .filter(|p| *p > 0)
                    .ok_or_else(|| {
                        Error::invalid("Audio start requires a target application pid")
                    })?,
            ),
            "system" => {
                if args.get("pid").is_some() {
                    return Err(Error::invalid("System audio scope cannot also specify pid"));
                }
                None
            }
            _ => return Err(Error::invalid("Audio scope must be application or system")),
        };
        let duration = recording_duration(args)?;
        if !unsafe { super::macos::CGPreflightScreenCaptureAccess() } {
            return Err(Error::new(
                -32003,
                "Grant Screen Recording permission before starting application loopback audio",
            ));
        }
        let collector = Arc::new(Mutex::new(Collector {
            samples: vec![],
            limit: RATE as usize * duration as usize / 1000,
            error: None,
            finished: false,
        }));
        let (stop, commands) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let worker_state = collector.clone();
        let worker = thread::spawn(move || {
            let operation = (|| -> Result<()> {
                let built = build(pid, worker_state.clone())?;
                if commands.try_recv().is_ok() {
                    return Err(Error::action("Audio start cancelled"));
                }
                completion(|callback| unsafe {
                    built
                        .stream
                        .startCaptureWithCompletionHandler(Some(callback))
                })?;
                let _ = ready.send(Ok(()));
                let deadline = Instant::now() + Duration::from_millis(duration);
                loop {
                    if Instant::now() >= deadline || commands.try_recv().is_ok() {
                        break;
                    }
                    if worker_state.lock().map_or(true, |s| {
                        s.error.is_some() || s.samples.len() / 2 >= s.limit
                    }) {
                        break;
                    }
                    pump(Duration::from_millis(10));
                }
                let result = completion(|callback| unsafe {
                    built
                        .stream
                        .stopCaptureWithCompletionHandler(Some(callback))
                });
                let removed = unsafe {
                    built.stream.removeStreamOutput_type_error(
                        ProtocolObject::from_ref(&*built.output),
                        SCStreamOutputType::Audio,
                    )
                }
                .map_err(|e| Error::action(e.localizedDescription().to_string()));
                result?;
                removed?;
                Ok(())
            })();
            if let Err(error) = &operation {
                let _ = ready.send(Err(error.clone()));
            }
            if let Ok(mut state) = worker_state.lock() {
                state.finished = true;
                if let Err(error) = operation {
                    state.error = Some(error.message);
                }
            }
        });
        let recording = Recording {
            owner: owner.into(),
            pid,
            collector,
            stop,
            worker: Some(worker),
        };
        match started.recv_timeout(Duration::from_secs(22)) {
            Ok(Ok(())) => {
                self.active = Some(recording);
                Ok(self.status())
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(Error::action("Audio start timed out")),
        }
    }
    fn stop(&mut self, owner: &str) -> Result<Value> {
        self.check_owner(owner)?;
        let mut recording = self
            .active
            .take()
            .ok_or_else(|| Error::action("No recording exists"))?;
        let _ = recording.stop.send(());
        if let Some(worker) = recording.worker.take() {
            worker
                .join()
                .map_err(|_| Error::action("Audio controller panicked"))?;
        }
        let state = recording
            .collector
            .lock()
            .map_err(|_| Error::action("Audio collector poisoned"))?;
        if let Some(error) = &state.error {
            return Err(Error::action(error.clone()));
        }
        if state.samples.is_empty() {
            return Err(Error::action("Audio recording produced no samples"));
        }
        let bytes = wav(&state.samples)?;
        Ok(
            json!({"mime_type":"audio/wav","data":base64::engine::general_purpose::STANDARD.encode(bytes),"sample_rate":RATE,"channels":CHANNELS,"frames":state.samples.len()/2,"duration_ms":state.samples.len() as u64*1000/(RATE as u64*2)}),
        )
    }
    pub fn end_session(&mut self, owner: &str) -> Result<()> {
        self.check_owner(owner)?;
        self.active.take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_owner_blocks_other_readers_and_stop_consumes_once() {
        let (stop, _) = mpsc::channel();
        let recording = Recording {
            owner: "a".into(),
            pid: Some(123),
            collector: Arc::new(Mutex::new(Collector {
                samples: vec![1, -1, 2, -2],
                limit: 2,
                error: None,
                finished: true,
            })),
            stop,
            worker: None,
        };
        let mut audio = Audio {
            active: Some(recording),
        };
        for method in ["status", "stop"] {
            assert_eq!(
                audio.execute(method, "b", &json!({})).unwrap_err().code,
                -32001
            );
        }
        assert!(audio.end_session("b").is_err());
        assert!(audio.active.is_some());
        let result = audio.execute("stop", "a", &json!({})).unwrap();
        assert_eq!(result["frames"], 2);
        assert_eq!(result["mime_type"], "audio/wav");
        assert!(audio.active.is_none());
        assert!(audio.execute("stop", "a", &json!({})).is_err());
    }
    #[test]
    fn recording_end_session_discards_owned_pcm_and_errors_are_not_reported_as_audio() {
        for failed in [false, true] {
            let (stop, _) = mpsc::channel();
            let recording = Recording {
                owner: "a".into(),
                pid: Some(123),
                collector: Arc::new(Mutex::new(Collector {
                    samples: vec![1, -1],
                    limit: 1,
                    error: failed.then(|| "capture failure".into()),
                    finished: true,
                })),
                stop,
                worker: None,
            };
            let mut audio = Audio {
                active: Some(recording),
            };
            if failed {
                assert_eq!(
                    audio.execute("stop", "a", &json!({})).unwrap_err().message,
                    "capture failure"
                );
            } else {
                audio.end_session("a").unwrap();
            }
            assert!(audio.active.is_none());
        }
    }
}
