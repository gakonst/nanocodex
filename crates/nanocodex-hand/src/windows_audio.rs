//! Default render-endpoint WASAPI loopback. Never opens a microphone endpoint.
#![allow(unsafe_code)]
use crate::Error;
use std::{
    collections::VecDeque,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Media::Audio::*,
        System::Com::{
            CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
        },
    },
    core::Interface,
};
type Result<T> = std::result::Result<T, Error>;
const FRAME_BYTES: usize = 4;
const CHUNK_BYTES: usize = 960 * FRAME_BYTES;
const QUEUE_BYTES: usize = CHUNK_BYTES * 4;
const PERIOD: Duration = Duration::from_millis(20);
fn error(message: impl Into<String>) -> Error {
    std::io::Error::other(message.into()).into()
}
fn win<T>(result: windows::core::Result<T>, action: &str) -> Result<T> {
    result.map_err(|e| {
        error(format!(
            "WASAPI {action}: {e} (HRESULT 0x{:08x})",
            e.code().0 as u32
        ))
    })
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}
struct Loopback {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    _device: IMMDevice,
    _enumerator: IMMDeviceEnumerator,
    _apartment: Apartment,
    capacity: u32,
    running: bool,
}
impl Loopback {
    fn open() -> Result<Self> {
        crate::ensure_interactive_session()?;
        // SAFETY: this synchronous function owns the calling MTA thread for the
        // capture lifetime, and balances every successful CoInitializeEx.
        win(
            unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok(),
            "initialize MTA",
        )?;
        let apartment = Apartment;
        let enumerator: IMMDeviceEnumerator = win(
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) },
            "enumerate render endpoints",
        )?;
        let device = win(
            unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) },
            "open default speakers (an active render endpoint is required)",
        )?;
        if win(unsafe { device.GetState() }, "read endpoint state")? != DEVICE_STATE_ACTIVE {
            return Err(error("Windows system audio requires active speakers"));
        }
        let client: IAudioClient = win(
            unsafe { device.Activate(CLSCTX_ALL, None) },
            "activate render audio client",
        )?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 192_000,
            nBlockAlign: FRAME_BYTES as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        win(
            unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK
                        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
                        | AUDCLNT_STREAMFLAGS_NOPERSIST,
                    1_000_000,
                    0,
                    &format,
                    None,
                )
            },
            "initialize 48 kHz stereo render loopback",
        )?;
        let capacity = win(
            unsafe { client.GetBufferSize() },
            "get endpoint buffer size",
        )?;
        if capacity == 0 || capacity > 48_000 {
            return Err(error("Windows audio endpoint buffer exceeds one second"));
        }
        let capture = win(
            unsafe { client.GetService() },
            "get loopback capture client",
        )?;
        let mut stream = Self {
            capture,
            client,
            _device: device,
            _enumerator: enumerator,
            _apartment: apartment,
            capacity,
            running: false,
        };
        win(unsafe { stream.client.Start() }, "start render loopback")?;
        stream.running = true;
        Ok(stream)
    }
    fn packet(&self, queue: &mut PcmQueue) -> Result<bool> {
        let queued = win(
            unsafe { self.capture.GetNextPacketSize() },
            "read next packet size",
        )?;
        if queued == 0 {
            return Ok(false);
        }
        if queued > self.capacity {
            return Err(error("Windows audio packet exceeds endpoint buffer"));
        }
        let mut data = std::ptr::null_mut();
        let mut frames = 0;
        let mut flags = 0;
        // The typed GetBuffer wrapper discards successful BUFFER_EMPTY status.
        let status = unsafe {
            (self.capture.vtable().GetBuffer)(
                self.capture.as_raw(),
                &mut data,
                &mut frames,
                &mut flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        win(status.ok(), "acquire audio packet")?;
        if status == AUDCLNT_S_BUFFER_EMPTY {
            return Ok(false);
        }
        let packet = Packet(&self.capture, frames);
        let result = (|| {
            if frames == 0 || frames > self.capacity || flags & !7 != 0 {
                return Err(error("invalid Windows audio packet length or flags"));
            }
            let length = frames as usize * FRAME_BYTES;
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                queue.bytes.clear(); // Never replay old buffered audio across a discontinuity.
            }
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                queue.silence(length);
            } else {
                if data.is_null() {
                    return Err(error("Windows audio packet has no PCM data"));
                }
                // SAFETY: WASAPI owns exactly frames * block-align bytes until
                // ReleaseBuffer; copy into bounded owned storage before releasing.
                queue.push(unsafe { std::slice::from_raw_parts(data, length) });
            }
            Ok(true)
        })();
        let released = win(
            unsafe { self.capture.ReleaseBuffer(frames) },
            "release audio packet",
        );
        std::mem::forget(packet);
        result.and(released.map(|_| true))
    }
}
impl Drop for Loopback {
    fn drop(&mut self) {
        if self.running {
            let _ = unsafe { self.client.Stop() };
        }
    }
}
struct Packet<'a>(&'a IAudioCaptureClient, u32);
impl Drop for Packet<'_> {
    fn drop(&mut self) {
        let _ = unsafe { self.0.ReleaseBuffer(self.1) };
    }
}
#[derive(Default)]
struct PcmQueue {
    bytes: VecDeque<u8>,
}
impl PcmQueue {
    fn reserve(&mut self, count: usize) -> usize {
        let count = count.min(QUEUE_BYTES);
        let discard = (self.bytes.len() + count).saturating_sub(QUEUE_BYTES);
        self.bytes.drain(..discard);
        count
    }
    fn push(&mut self, bytes: &[u8]) {
        let keep = self.reserve(bytes.len());
        self.bytes.extend(&bytes[bytes.len() - keep..]);
    }
    fn silence(&mut self, bytes: usize) {
        let keep = self.reserve(bytes);
        self.bytes.extend(std::iter::repeat_n(0, keep));
    }
    fn chunk(&mut self) -> [u8; CHUNK_BYTES] {
        let mut output = [0; CHUNK_BYTES];
        for byte in &mut output {
            *byte = self.bytes.pop_front().unwrap_or(0);
        }
        output
    }
}
/// Write 48 kHz stereo signed 16-bit little-endian system audio on a dedicated
/// blocking thread. Silent periods produce PCM zeros on the same 20 ms clock.
/// The caller must unblock/close its writer when stopping; arbitrary Write
/// implementations can otherwise block independently of WASAPI cancellation.
pub fn capture_audio(mut writer: impl Write, stop: Arc<AtomicBool>) -> Result<()> {
    if stop.load(Ordering::Acquire) {
        return Ok(());
    }
    let stream = Loopback::open()?;
    let mut queue = PcmQueue::default();
    let mut next = Instant::now() + PERIOD;
    while !stop.load(Ordering::Acquire) {
        // A bounded drain prevents a pathological device from starving stop.
        for _ in 0..32 {
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if !stream.packet(&mut queue)? {
                break;
            }
        }
        let now = Instant::now();
        if now >= next {
            writer.write_all(&queue.chunk())?;
            // Never burst to catch up after downstream backpressure. One 20 ms
            // sample per real period bounds combined real + synthesized PCM.
            next += PERIOD;
            let after_write = Instant::now();
            if next <= after_write {
                next = after_write + PERIOD;
            }
        }
        std::thread::sleep(
            next.saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5)),
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_backlog_drops_whole_stereo_frames() {
        let mut queue = PcmQueue::default();
        queue.push(&vec![1; QUEUE_BYTES]);
        queue.push(&vec![2; CHUNK_BYTES]);
        assert_eq!(queue.bytes.len(), QUEUE_BYTES);
        for _ in 0..3 {
            assert_eq!(queue.chunk(), [1; CHUNK_BYTES]);
        }
        assert_eq!(queue.chunk(), [2; CHUNK_BYTES]);
        assert_eq!(queue.chunk(), [0; CHUNK_BYTES]);
    }
    #[test]
    fn silence_and_oversized_packets_stay_bounded() {
        let mut queue = PcmQueue::default();
        queue.push(&[7; 4]);
        let chunk = queue.chunk();
        assert_eq!(&chunk[..4], &[7; 4]);
        assert!(chunk[4..].iter().all(|b| *b == 0));
        queue.push(&vec![9; QUEUE_BYTES * 2]);
        assert_eq!(queue.bytes.len(), QUEUE_BYTES);
        queue.silence(QUEUE_BYTES * 2);
        assert_eq!(queue.bytes.len(), QUEUE_BYTES);
        assert_eq!(queue.chunk(), [0; CHUNK_BYTES]);
    }
    #[test]
    fn cancellation_before_start_never_opens_a_device() {
        capture_audio(Vec::new(), Arc::new(AtomicBool::new(true))).unwrap();
    }
}
