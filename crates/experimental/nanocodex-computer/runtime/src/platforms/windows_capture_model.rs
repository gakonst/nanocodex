//! Platform-neutral WGC bounds, frame generations and capture-thread ownership.
//! Synthetic tests exercise these contracts; they do not execute Windows APIs.
use super::windows_uia_model::Root;
use crate::{Error, Result};
use image::ImageEncoder;
use std::{
    io::{self, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub const MAX_PIXELS: usize = 40_000_000;
pub const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RESIZES: u32 = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}
impl Size {
    pub fn new(width: i32, height: i32) -> Result<Self> {
        if width <= 0 || height <= 0 || width > 16384 || height > 16384 {
            return Err(Error::action(
                "WGC content dimensions are invalid or exceed bounds",
            ));
        }
        let size = Self {
            width: width as u32,
            height: height as u32,
        };
        size.bytes()?;
        Ok(size)
    }
    pub fn bytes(self) -> Result<usize> {
        let pixels = (self.width as usize)
            .checked_mul(self.height as usize)
            .filter(|n| *n > 0 && *n <= MAX_PIXELS && self.width <= 16384 && self.height <= 16384)
            .ok_or_else(|| Error::action("WGC pixel allocation exceeds bounds"))?;
        Ok(pixels * 4)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Physical desktop pixels: left, top, width, height.
    pub window: [i32; 4],
    pub content: [i32; 4],
}
impl Geometry {
    pub fn validate(self) -> Result<(Size, Size, [usize; 2])> {
        let outer = Size::new(self.window[2], self.window[3])?;
        let inner = Size::new(self.content[2], self.content[3])?;
        let x = i64::from(self.content[0]) - i64::from(self.window[0]);
        let y = i64::from(self.content[1]) - i64::from(self.window[1]);
        if x < 0
            || y < 0
            || x + i64::from(inner.width) > i64::from(outer.width)
            || y + i64::from(inner.height) > i64::from(outer.height)
        {
            return Err(Error::action(
                "WGC content frame is outside the physical window frame",
            ));
        }
        Ok((outer, inner, [x as usize, y as usize]))
    }
    pub fn frame(self) -> [f64; 4] {
        self.window.map(f64::from)
    }
}
pub fn mapped_len(size: Size, row_pitch: usize) -> Result<usize> {
    let row = size.bytes()? / size.height as usize;
    if row_pitch < row {
        return Err(Error::action(
            "WGC mapped row pitch is shorter than the pixel row",
        ));
    }
    row_pitch
        .checked_mul(size.height as usize - 1)
        .and_then(|v| v.checked_add(row))
        .filter(|v| *v <= MAX_PIXELS * 8 && *v <= isize::MAX as usize)
        .ok_or_else(|| Error::action("WGC mapped row pitch exceeds memory bounds"))
}
/// Copy only declared content pixels. Undefined texture/pitch padding never
/// enters the image; invisible window borders remain transparent zeroes.
pub fn bgra_to_window_rgba(geometry: Geometry, row_pitch: usize, data: &[u8]) -> Result<Vec<u8>> {
    let (outer, inner, [x, y]) = geometry.validate()?;
    let length = mapped_len(inner, row_pitch)?;
    if data.len() < length {
        return Err(Error::action("WGC mapped pixel buffer is truncated"));
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(outer.bytes()?)
        .map_err(|_| Error::action("WGC pixel allocation failed"))?;
    rgba.resize(outer.bytes()?, 0);
    for row in 0..inner.height as usize {
        let source = &data[row * row_pitch..row * row_pitch + inner.width as usize * 4];
        let start = ((y + row) * outer.width as usize + x) * 4;
        for (from, to) in source
            .chunks_exact(4)
            .zip(rgba[start..].chunks_exact_mut(4))
        {
            if from[3] != 0 {
                to.copy_from_slice(&[from[2], from[1], from[0], from[3]]);
            }
        }
    }
    Ok(rgba)
}
struct BoundedBytes {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("WGC encoded PNG exceeds output bound"));
        }
        self.bytes
            .try_reserve(bytes.len())
            .map_err(|_| io::Error::other("WGC PNG allocation failed"))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn encode_png(size: Size, rgba: &[u8], limit: usize) -> Result<Vec<u8>> {
    if rgba.len() != size.bytes()? || limit == 0 || limit > MAX_ENCODED_BYTES {
        return Err(Error::invalid("WGC PNG pixels or output limit are invalid"));
    }
    let mut out = BoundedBytes {
        bytes: Vec::new(),
        limit,
    };
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(
            rgba,
            size.width,
            size.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| Error::action(format!("WGC PNG encoding failed: {e}")))?;
    Ok(out.bytes)
}
#[derive(Debug, PartialEq, Eq)]
pub enum FrameDecision {
    Accept,
    Recreate(Size),
    Discard,
}
pub struct Frames {
    pub size: Size,
    pub generation: u32,
    last_time: i64,
}
impl Frames {
    pub fn new(size: Size) -> Result<Self> {
        size.bytes()?;
        Ok(Self {
            size,
            generation: 0,
            last_time: -1,
        })
    }
    pub fn observe(&mut self, size: Size, time: i64) -> Result<FrameDecision> {
        size.bytes()?;
        if time < 0 {
            return Err(Error::action("WGC frame timestamp is invalid"));
        }
        if time <= self.last_time {
            return Ok(FrameDecision::Discard);
        }
        self.last_time = time;
        if size != self.size {
            if self.generation >= MAX_RESIZES {
                return Err(Error::action(
                    "WGC window did not stabilize within resize bound",
                ));
            }
            self.generation += 1;
            self.size = size;
            return Ok(FrameDecision::Recreate(size));
        }
        Ok(FrameDecision::Accept)
    }
}
#[derive(Debug)]
pub struct Screenshot {
    pub root: Root,
    pub geometry: Geometry,
    pub generation: u32,
    pub png: Vec<u8>,
}
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub root: Root,
    pub geometry: Geometry,
    pub generation: u32,
}
impl Binding {
    pub fn validate(self, root: Root, geometry: Geometry) -> Result<()> {
        if self.root != root || self.geometry != geometry {
            return Err(Error::action(
                "WGC screenshot target or physical/content geometry is stale",
            ));
        }
        geometry.validate()?;
        Ok(())
    }
}
impl Screenshot {
    pub fn binding(&self) -> Binding {
        Binding {
            root: self.root,
            geometry: self.geometry,
            generation: self.generation,
        }
    }
}
pub struct Context {
    deadline: Instant,
    closed: Arc<AtomicBool>,
}
impl Context {
    pub fn check(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(Error::new(
                -32008,
                "WGC capture expired or owner was closed",
            ));
        }
        Ok(())
    }
    pub fn wait_quantum(&self) -> Duration {
        self.deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(10))
    }
}
/// Constructed/dropped on its worker; no native COM objects cross the channel.
pub trait Provider {
    fn capture(&mut self, root: Root, ctx: &Context) -> Result<Screenshot>;
}
struct Request {
    root: Root,
    deadline: Instant,
    reply: SyncSender<Result<Screenshot>>,
}
pub struct Worker {
    send: Option<SyncSender<Request>>,
    closed: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    timeout: Duration,
}
impl Worker {
    pub fn spawn(
        factory: impl FnOnce() -> Result<Box<dyn Provider>> + Send + 'static,
        timeout: Duration,
    ) -> Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(Error::invalid("Invalid WGC worker timeout"));
        }
        let (send, receive) = mpsc::sync_channel::<Request>(1);
        let (ready, ready_rx) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let thread = thread::Builder::new()
            .name("skyre-wgc-mta".into())
            .spawn(move || {
                let mut provider = match factory() {
                    Ok(p) => {
                        let _ = ready.send(Ok(()));
                        p
                    }
                    Err(e) => {
                        let _ = ready.send(Err(e));
                        return;
                    }
                };
                while let Ok(request) = receive.recv() {
                    let ctx = Context {
                        deadline: request.deadline,
                        closed: flag.clone(),
                    };
                    let result = ctx
                        .check()
                        .and_then(|()| provider.capture(request.root, &ctx))
                        .and_then(|shot| {
                            ctx.check()?;
                            if shot.root != request.root
                                || shot.png.len() > MAX_ENCODED_BYTES
                                || shot.png.is_empty()
                            {
                                return Err(Error::action(
                                    "WGC provider returned an invalid target or image bound",
                                ));
                            }
                            shot.geometry.validate()?;
                            Ok(shot)
                        });
                    if result.as_ref().is_err_and(|e| e.code == -32008) {
                        flag.store(true, Ordering::Release);
                    }
                    let _ = request.reply.send(result);
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                }
            })?;
        let mut worker = Self {
            send: Some(send),
            closed,
            thread: Some(thread),
            timeout,
        };
        match ready_rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(worker),
            Ok(Err(error)) => {
                worker.close();
                Err(error)
            }
            Err(_) => {
                worker.close();
                Err(Error::new(
                    -32008,
                    "WGC MTA initialization timed out or disconnected",
                ))
            }
        }
    }
    pub fn capture(&mut self, root: Root) -> Result<Screenshot> {
        root.validate()?;
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::new(
                -32008,
                "WGC worker unavailable after timeout or shutdown",
            ));
        }
        let (reply, receive) = mpsc::sync_channel(1);
        self.send
            .as_ref()
            .ok_or_else(|| Error::action("WGC worker closed"))?
            .try_send(Request {
                root,
                deadline: Instant::now() + self.timeout,
                reply,
            })
            .map_err(|_| Error::action("WGC worker disconnected or request queue full"))?;
        match receive.recv_timeout(self.timeout) {
            Ok(result) => {
                if result.as_ref().is_err_and(|e| e.code == -32008) {
                    self.close();
                }
                result
            }
            Err(_) => {
                self.close();
                Err(Error::new(-32008, "WGC capture timed out; worker disabled"))
            }
        }
    }
    pub fn close(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.send.take();
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.close();
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
    }
}
