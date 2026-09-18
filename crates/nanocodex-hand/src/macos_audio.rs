//! ScreenCaptureKit system output only; microphone capture is never enabled.
#![allow(unsafe_code)]
use crate::Error;
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{
    AnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_core_audio_types::*;
use objc2_core_media::{CMAudioFormatDescriptionGetStreamBasicDescription, CMSampleBuffer};
use objc2_core_video::*;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::*;
use std::{
    io::Write,
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

type Result<T> = std::result::Result<T, Error>;
const MAX_FRAMES: usize = 4800;
fn error(s: impl Into<String>) -> Error {
    std::io::Error::other(s.into()).into()
}
fn pcm(sample: f32) -> [u8; 2] {
    (if sample.is_nan() {
        0
    } else {
        (sample.clamp(-1.0, 1.0) * 32768.0) as i16
    })
    .to_le_bytes()
}
fn convert(samples: &[f32], frames: usize, planar: bool) -> Vec<u8> {
    let mut result = Vec::with_capacity(frames * 4);
    for frame in 0..frames {
        for channel in 0..2 {
            result.extend_from_slice(&pcm(samples[if planar {
                channel * frames + frame
            } else {
                frame * 2 + channel
            }]));
        }
    }
    result
}
#[repr(C)]
struct StereoBuffers {
    count: u32,
    buffers: [AudioBuffer; 2],
}
// Copy while the CMSampleBuffer is valid. Both allocation and queue size have
// fixed upper bounds; the capture queue never waits on a writer or a mutex.
unsafe fn packet(sample: &CMSampleBuffer) -> Option<Vec<u8>> {
    let frames = usize::try_from(unsafe { sample.num_samples() }).ok()?;
    if frames == 0 || frames > MAX_FRAMES {
        return None;
    }
    let description = unsafe { sample.format_description() }?;
    let format =
        unsafe { CMAudioFormatDescriptionGetStreamBasicDescription(&description).as_ref() }?;
    if format.mFormatID != kAudioFormatLinearPCM
        || format.mSampleRate != 48000.0
        || format.mChannelsPerFrame != 2
        || format.mBitsPerChannel != 32
        || format.mFormatFlags & kAudioFormatFlagIsFloat == 0
        || format.mFormatFlags & kAudioFormatFlagIsBigEndian != 0
    {
        return None;
    }
    let planar = format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
    let mut samples = vec![0f32; frames * 2];
    let mut list = StereoBuffers {
        count: if planar { 2 } else { 1 },
        buffers: [
            AudioBuffer {
                mNumberChannels: if planar { 1 } else { 2 },
                mDataByteSize: (frames * if planar { 4 } else { 8 }) as u32,
                mData: samples.as_mut_ptr().cast(),
            },
            AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: (frames * 4) as u32,
                mData: unsafe { samples.as_mut_ptr().add(frames).cast() },
            },
        ],
    };
    if unsafe {
        sample.copy_pcm_data_into_audio_buffer_list(
            0,
            frames as i32,
            NonNull::from(&mut list).cast(),
        )
    } != 0
    {
        return None;
    }
    Some(convert(&samples, frames, planar))
}
struct Ivars {
    latest: Arc<std::sync::Mutex<Option<Vec<u8>>>>,
    video: Option<(usize, usize)>,
    sender: mpsc::SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
}
define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = Ivars]
    struct AudioOutput;
    unsafe impl NSObjectProtocol for AudioOutput {}
    unsafe impl SCStreamDelegate for AudioOutput {
        #[unsafe(method(stream:didStopWithError:))]
        unsafe fn stopped(&self, _stream: &SCStream, _error: &NSError) {
            self.ivars().stop.store(true, Ordering::Release);
        }
    }
    unsafe impl SCStreamOutput for AudioOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if self.ivars().stop.load(Ordering::Acquire) {
                return;
            }
            let bytes = match self.ivars().video {
                Some((w, h)) if kind == SCStreamOutputType::Screen => unsafe {
                    video_packet(sample, w, h)
                },
                None if kind == SCStreamOutputType::Audio => unsafe { packet(sample) },
                _ => None,
            };
            if let Some(bytes) = bytes {
                if self.ivars().video.is_some() {
                    if let Ok(mut latest) = self.ivars().latest.try_lock() {
                        *latest = Some(bytes);
                    }
                } else {
                    let _ = self.ivars().sender.try_send(bytes);
                }
            }
        }
    }
);
struct Stream {
    kind: SCStreamOutputType,
    stream: Retained<SCStream>,
    output: Retained<AudioOutput>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            self.stream.stopCaptureWithCompletionHandler(None);
            let _ = self
                .stream
                .removeStreamOutput_type_error(ProtocolObject::from_ref(&*self.output), self.kind);
        }
    }
}
/// Writes stereo 48 kHz s16le on a blocking worker. The caller must close or
/// unblock its writer on cancellation. Audio callbacks only use try_send.
pub fn capture_audio(writer: impl Write, stop: Arc<AtomicBool>) -> Result<()> {
    capture(writer, stop, None)
}
fn capture(
    mut writer: impl Write,
    stop: Arc<AtomicBool>,
    video: Option<(usize, usize)>,
) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    if !objc2_core_graphics::CGPreflightScreenCaptureAccess() {
        return Err(error(
            "macOS system audio requires Screen & System Audio Recording permission",
        ));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    let callback = RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let result = if !err.is_null() {
            Err(unsafe { &*err }.to_string())
        } else {
            unsafe { Retained::retain(content) }.ok_or_else(|| "no shareable content".into())
        };
        let _ = tx.try_send(result);
    });
    unsafe {
        SCShareableContent::getShareableContentWithCompletionHandler(&callback);
    }
    let content = rx.recv_timeout(Duration::from_secs(2))?.map_err(error)?;
    let displays = unsafe { content.displays() };
    let display = (0..displays.count())
        .map(|i| displays.objectAtIndex(i))
        .find(|d| unsafe { d.displayID() } == objc2_core_graphics::CGMainDisplayID())
        .ok_or_else(|| error("main display unavailable"))?;
    let (tx, rx) = mpsc::sync_channel(if video.is_some() { 1 } else { 4 });
    unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::new(),
        );
        let config = SCStreamConfiguration::new();
        config.setCapturesAudio(video.is_none());
        if let Some((width, height)) = video {
            config.setWidth(width);
            config.setHeight(height);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setMinimumFrameInterval(objc2_core_media::CMTime::new(1, 60));
        }
        config.setSampleRate(48000);
        config.setChannelCount(2);
        config.setExcludesCurrentProcessAudio(false);
        let shared_latest = Arc::new(std::sync::Mutex::new(None));
        let output: Retained<AudioOutput> = msg_send![
            super(AudioOutput::alloc().set_ivars(Ivars {
                latest: shared_latest.clone(),
                video,
                sender: tx,
                stop: stop.clone()
            })),
            init
        ];
        let kind = if video.is_some() {
            SCStreamOutputType::Screen
        } else {
            SCStreamOutputType::Audio
        };
        let stream = Stream {
            kind,
            stream: SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                Some(ProtocolObject::from_ref(&*output)),
            ),
            output,
        };
        let queue = DispatchQueue::new("com.nanocodex.system-audio", None);
        stream
            .stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*stream.output),
                kind,
                Some(&queue),
            )
            .map_err(|e| error(e.to_string()))?;
        let (started, wait) = mpsc::sync_channel(1);
        let completion = RcBlock::new(move |err: *mut NSError| {
            let _ = started.try_send(if err.is_null() {
                Ok(())
            } else {
                Err((&*err).to_string())
            });
        });
        stream
            .stream
            .startCaptureWithCompletionHandler(Some(&completion));
        wait.recv_timeout(Duration::from_secs(2))?.map_err(error)?;
        let mut latest = None;
        let mut next = std::time::Instant::now();
        while !stop.load(Ordering::Acquire) {
            if video.is_some() {
                if let Ok(mut slot) = shared_latest.lock()
                    && let Some(bytes) = slot.take()
                {
                    latest = Some(bytes);
                }
                if let Some(bytes) = &latest {
                    writer.write_all(bytes)?;
                }
                next += Duration::from_nanos(1_000_000_000 / 60);
                let now = std::time::Instant::now();
                if next < now {
                    next = now;
                }
                std::thread::sleep(next.saturating_duration_since(now));
                continue;
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(bytes) => writer.write_all(&bytes)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(error("system audio disconnected"));
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn float_layouts_and_clipping() {
        let interleaved = [0.0, 1.0, -1.0, 0.5];
        assert_eq!(
            convert(&interleaved, 2, false),
            convert(&[0.0, -1.0, 1.0, 0.5], 2, true)
        );
        assert_eq!(
            convert(&interleaved, 2, false),
            [0, 0, 255, 127, 0, 128, 0, 64]
        );
        assert_eq!(pcm(f32::NAN), [0, 0]);
    }
    #[test]
    fn cancellation_before_start() {
        capture_audio(Vec::new(), Arc::new(AtomicBool::new(true))).unwrap();
    }
    #[test]
    #[ignore = "requires macOS screen recording permission; local frame counts only"]
    fn native_video_stops() {
        struct Count(usize);
        impl Write for Count {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut sink = Count(0);
            capture_video(&mut sink, flag, 640, 360).map(|()| sink.0)
        });
        std::thread::sleep(Duration::from_secs(3));
        stop.store(true, Ordering::Release);
        let bytes = worker.join().unwrap().unwrap();
        assert!(bytes >= 640 * 360 * 4, "no native BGRA frames received");
        assert_eq!(bytes % (640 * 360 * 4), 0);
        eprintln!(
            "captured {} complete 640x360 BGRA frames",
            bytes / (640 * 360 * 4)
        );
    }
    #[test]
    #[ignore = "requires macOS screen recording permission; captures system audio locally"]
    fn native_capture_stops() {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let worker = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            capture_audio(&mut bytes, flag).map(|()| bytes.len())
        });
        std::thread::sleep(Duration::from_secs(3));
        stop.store(true, Ordering::Release);
        let count = worker.join().unwrap().unwrap();
        assert!(count > 0, "no system PCM received");
        eprintln!("captured {count} bytes of 48000 Hz stereo s16le");
    }
}

/// Physical main-display dimensions, with no upscaling.
pub fn main_display_dimensions() -> Result<(usize, usize)> {
    let display = objc2_core_graphics::CGMainDisplayID();
    let size = (
        objc2_core_graphics::CGDisplayPixelsWide(display),
        objc2_core_graphics::CGDisplayPixelsHigh(display),
    );
    if size.0 == 0 || size.1 == 0 {
        return Err(error("main display unavailable"));
    }
    Ok(size)
}
/// Packed BGRA frames at 60 Hz. Repeats the latest frame for static desktops.
/// Caller must close/unblock the writer when requesting cancellation.
pub fn capture_video(
    writer: impl Write,
    stop: Arc<AtomicBool>,
    width: usize,
    height: usize,
) -> Result<()> {
    let native = main_display_dimensions()?;
    if width == 0
        || height == 0
        || width > 3840
        || height > 2160
        || width > native.0
        || height > native.1
    {
        return Err(error("invalid system video dimensions"));
    }
    capture(writer, stop, Some((width, height)))
}
unsafe fn video_packet(sample: &CMSampleBuffer, width: usize, height: usize) -> Option<Vec<u8>> {
    let buffer = unsafe { sample.image_buffer() }?;
    if CVPixelBufferGetWidth(&buffer) != width
        || CVPixelBufferGetHeight(&buffer) != height
        || CVPixelBufferGetPixelFormatType(&buffer) != kCVPixelFormatType_32BGRA
    {
        return None;
    }
    if unsafe { CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly) } != 0 {
        return None;
    }
    let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
    let stride = CVPixelBufferGetBytesPerRow(&buffer);
    let result = if base.is_null() || stride < width * 4 {
        None
    } else {
        let mut bytes = Vec::with_capacity(width * height * 4);
        for row in 0..height {
            bytes.extend_from_slice(unsafe {
                std::slice::from_raw_parts(base.add(row * stride), width * 4)
            });
        }
        Some(bytes)
    };
    unsafe {
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags::ReadOnly);
    }
    result
}
