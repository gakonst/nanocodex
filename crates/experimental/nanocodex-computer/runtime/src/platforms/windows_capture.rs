//! HWND-scoped Windows Graphics Capture. No desktop-rectangle/GDI fallback.
//! All WinRT/D3D resources live on one capture MTA; callbacks only signal it.
use super::{
    windows_capture_model::{
        self as model, Context, FrameDecision, Frames, Geometry, Provider, Screenshot, Size, Worker,
    },
    windows_uia_model::Root,
};
use crate::{Error, Result};
use std::{
    ffi::c_void,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Duration,
};
use windows::{
    Foundation::TypedEventHandler,
    Graphics::{
        Capture::{
            Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
            GraphicsCaptureSession,
        },
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
        SizeInt32,
    },
    Win32::{
        Foundation::{HMODULE, HWND, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_HARDWARE,
            Direct3D11::*,
            Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
            Dxgi::{Common::*, DXGI_ERROR_WAS_STILL_DRAWING, IDXGIDevice},
        },
        System::WinRT::{
            Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
            Graphics::Capture::IGraphicsCaptureItemInterop,
            RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
        },
        UI::{
            HiDpi::{
                DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                SetThreadDpiAwarenessContext,
            },
            WindowsAndMessaging::{
                GetWindowDisplayAffinity, GetWindowRect, GetWindowThreadProcessId, IsIconic,
                IsWindow, IsWindowVisible,
            },
        },
    },
    core::{IInspectable, Interface, factory},
};
fn win<T>(value: windows::core::Result<T>, step: &str) -> Result<T> {
    value.map_err(|e| {
        Error::action(format!(
            "WGC {step} failed (HRESULT 0x{:08x}): {e}",
            e.code().0 as u32
        ))
    })
}
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}
struct Dpi(Option<DPI_AWARENESS_CONTEXT>);
impl Dpi {
    fn enter() -> Result<Self> {
        let old =
            unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if old.0.is_null() {
            return Err(Error::action(
                "Cannot establish physical-pixel thread DPI context",
            ));
        }
        Ok(Self(Some(old)))
    }
    fn restore(&mut self) -> Result<()> {
        if let Some(old) = self.0.take()
            && unsafe { SetThreadDpiAwarenessContext(old) }.0.is_null()
        {
            return Err(Error::action(
                "Cannot restore the previous thread DPI context",
            ));
        }
        Ok(())
    }
}
impl Drop for Dpi {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}
fn physical<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let mut dpi = Dpi::enter()?;
    let result = operation();
    let restored = dpi.restore();
    match result {
        Ok(value) => {
            restored?;
            Ok(value)
        }
        Err(error) => Err(with_cleanup(error, restored.err())),
    }
}
fn convert(rect: RECT) -> Result<[i32; 4]> {
    let w = rect
        .right
        .checked_sub(rect.left)
        .ok_or_else(|| Error::action("Window width overflow"))?;
    let h = rect
        .bottom
        .checked_sub(rect.top)
        .ok_or_else(|| Error::action("Window height overflow"))?;
    Size::new(w, h)?;
    Ok([rect.left, rect.top, w, h])
}
/// Used by screenshot IDs and pointer conversion as well as WGC, so neither
/// path mixes DPI-virtualized window rectangles with physical capture pixels.
pub fn window_frame(hwnd: usize) -> Result<[f64; 4]> {
    physical(|| {
        let mut rect = RECT::default();
        win(
            unsafe { GetWindowRect(HWND(hwnd as *mut c_void), &mut rect) },
            "GetWindowRect",
        )?;
        Ok(convert(rect)?.map(f64::from))
    })
}
pub fn geometry(root: Root) -> Result<Geometry> {
    root.validate()?;
    let hwnd = HWND(root.hwnd as *mut c_void);
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() || pid != root.pid {
        return Err(Error::action("WGC target HWND/PID changed"));
    }
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() || unsafe { IsIconic(hwnd) }.as_bool() {
        return Err(Error::action("WGC target window is hidden or minimized"));
    }
    physical(|| {
        let mut outer = RECT::default();
        let mut inner = RECT::default();
        let mut cloaked = 0u32;
        win(unsafe { GetWindowRect(hwnd, &mut outer) }, "window bounds")?;
        win(
            unsafe {
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_EXTENDED_FRAME_BOUNDS,
                    (&mut inner as *mut RECT).cast(),
                    size_of::<RECT>() as u32,
                )
            },
            "visible frame bounds",
        )?;
        win(
            unsafe {
                DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    (&mut cloaked as *mut u32).cast(),
                    size_of::<u32>() as u32,
                )
            },
            "window cloaking state",
        )?;
        if cloaked != 0 {
            return Err(Error::action("WGC target window is cloaked"));
        }
        let mut affinity = 0;
        // This optional API is documented to fail for non-layered windows. A
        // positive protection result is enforced; failure is not permission proof.
        if unsafe { GetWindowDisplayAffinity(hwnd, &mut affinity) }.is_ok() && affinity != 0 {
            return Err(Error::action("WGC target window excludes screen capture"));
        }
        let geometry = Geometry {
            window: convert(outer)?,
            content: convert(inner)?,
        };
        geometry.validate()?;
        Ok(geometry)
    })
}
struct NativeProvider {
    _apartment: Apartment,
}
impl Provider for NativeProvider {
    fn capture(&mut self, root: Root, ctx: &Context) -> Result<Screenshot> {
        capture(root, ctx)
    }
}
pub fn start() -> Result<Worker> {
    Worker::spawn(
        || {
            win(
                unsafe { RoInitialize(RO_INIT_MULTITHREADED) },
                "RoInitialize",
            )?;
            let apartment = Apartment;
            if !win(GraphicsCaptureSession::IsSupported(), "IsSupported")? {
                return Err(Error::unsupported(
                    "Windows Graphics Capture is unavailable on this device",
                ));
            }
            Ok(Box::new(NativeProvider {
                _apartment: apartment,
            }))
        },
        Duration::from_secs(10),
    )
}
struct Device {
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    projected: IDirect3DDevice,
}
impl Device {
    fn new() -> Result<Self> {
        let mut device = None;
        let mut context = None;
        win(
            unsafe {
                D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            },
            "D3D11CreateDevice",
        )?;
        let device = device.ok_or_else(|| Error::action("WGC D3D device is missing"))?;
        let context = context.ok_or_else(|| Error::action("WGC D3D context is missing"))?;
        let dxgi: IDXGIDevice = win(device.cast(), "IDXGIDevice")?;
        let projected = win(
            win(
                unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) },
                "project D3D device",
            )?
            .cast(),
            "IDirect3DDevice",
        )?;
        Ok(Self {
            device,
            context,
            projected,
        })
    }
    fn check(&self) -> Result<()> {
        win(
            unsafe { self.device.GetDeviceRemovedReason() },
            "D3D device status",
        )
    }
}
struct Signals {
    active: AtomicBool,
    closed: AtomicBool,
    wake: SyncSender<()>,
}
impl Signals {
    fn signal(&self, closed: bool) {
        if self.active.load(Ordering::Acquire) {
            if closed {
                self.closed.store(true, Ordering::Release);
            }
            let _ = self.wake.try_send(()); // Coalesced wakeups, never COM work in callback.
        }
    }
    fn check(&self, ctx: &Context) -> Result<()> {
        ctx.check()?;
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::action("WGC target capture item closed"));
        }
        Ok(())
    }
}
struct Session {
    item: GraphicsCaptureItem,
    pool: Option<Direct3D11CaptureFramePool>,
    session: Option<GraphicsCaptureSession>,
    frame_token: Option<i64>,
    closed_token: Option<i64>,
    signals: Arc<Signals>,
}
impl Session {
    fn new(item: GraphicsCaptureItem, device: &Device, size: Size) -> Result<(Self, Receiver<()>)> {
        let (wake, receive) = mpsc::sync_channel(1);
        let signals = Arc::new(Signals {
            active: AtomicBool::new(true),
            closed: AtomicBool::new(false),
            wake,
        });
        let mut owner = Self {
            item,
            pool: None,
            session: None,
            frame_token: None,
            closed_token: None,
            signals,
        };
        let result = (|| {
            owner.pool = Some(win(
                Direct3D11CaptureFramePool::CreateFreeThreaded(
                    &device.projected,
                    DirectXPixelFormat::B8G8R8A8UIntNormalized,
                    2,
                    size_int(size),
                ),
                "CreateFreeThreaded",
            )?);
            let pool = owner.pool.as_ref().unwrap();
            let frame_signal = owner.signals.clone();
            owner.frame_token = Some(win(
                pool.FrameArrived(
                    &TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(
                        move |_, _| {
                            frame_signal.signal(false);
                            Ok(())
                        },
                    ),
                ),
                "subscribe FrameArrived",
            )?);
            let closed_signal = owner.signals.clone();
            owner.closed_token = Some(win(
                owner.item.Closed(
                    &TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
                        closed_signal.signal(true);
                        Ok(())
                    }),
                ),
                "subscribe Closed",
            )?);
            let session = win(
                pool.CreateCaptureSession(&owner.item),
                "CreateCaptureSession",
            )?;
            owner.session = Some(session);
            let session = owner.session.as_ref().unwrap();
            win(
                session.SetIsCursorCaptureEnabled(false),
                "disable cursor capture",
            )?;
            // Older session interfaces have no secondary-window capture option;
            // their item capture remains the default single-window behavior.
            if let Err(e) = session.SetIncludeSecondaryWindows(false)
                && e.code() != windows::Win32::Foundation::E_NOINTERFACE
            {
                return Err(Error::action(format!(
                    "WGC disable secondary windows failed: {e}"
                )));
            }
            win(session.StartCapture(), "StartCapture")
        })();
        if let Err(error) = result {
            let cleanup = owner.close();
            return Err(with_cleanup(error, cleanup.err()));
        }
        Ok((owner, receive))
    }
    fn close(&mut self) -> Result<()> {
        self.signals.active.store(false, Ordering::Release);
        let mut errors = Vec::new();
        if let Some(token) = self.frame_token.take()
            && let Some(pool) = &self.pool
            && let Err(e) = win(pool.RemoveFrameArrived(token), "remove FrameArrived")
        {
            errors.push(e);
        }
        if let Some(token) = self.closed_token.take()
            && let Err(e) = win(self.item.RemoveClosed(token), "remove Closed")
        {
            errors.push(e);
        }
        if let Some(session) = self.session.take()
            && let Err(e) = win(session.Close(), "close session")
        {
            errors.push(e);
        }
        if let Some(pool) = self.pool.take()
            && let Err(e) = win(pool.Close(), "close frame pool")
        {
            errors.push(e);
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::action(
                errors
                    .into_iter()
                    .map(|e| e.message)
                    .collect::<Vec<_>>()
                    .join("; "),
            ))
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
fn with_cleanup(mut error: Error, cleanup: Option<Error>) -> Error {
    if let Some(cleanup) = cleanup {
        error
            .message
            .push_str(&format!("; cleanup also failed: {}", cleanup.message));
    }
    error
}
fn size_int(size: Size) -> SizeInt32 {
    SizeInt32 {
        Width: size.width as i32,
        Height: size.height as i32,
    }
}
struct Frame(Option<Direct3D11CaptureFrame>);
impl Frame {
    fn close(&mut self) -> Result<()> {
        if let Some(frame) = self.0.take() {
            win(frame.Close(), "close frame")
        } else {
            Ok(())
        }
    }
}
impl Drop for Frame {
    fn drop(&mut self) {
        let _ = self.close();
    }
}
fn next_frame(pool: &Direct3D11CaptureFramePool) -> Result<Option<Frame>> {
    let mut raw = std::ptr::null_mut();
    // The typed projection treats S_OK/null (no queued frame) as E_POINTER.
    // Preserve real HRESULT failures and distinguish the documented empty case.
    win(
        unsafe { (pool.vtable().TryGetNextFrame)(pool.as_raw(), &mut raw).ok() },
        "TryGetNextFrame",
    )?;
    Ok(if raw.is_null() {
        None
    } else {
        Some(Frame(Some(unsafe {
            Direct3D11CaptureFrame::from_raw(raw)
        })))
    })
}
struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}
impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(self.texture, 0);
        }
    }
}
fn pixels(
    device: &Device,
    frame: &Direct3D11CaptureFrame,
    geometry: Geometry,
    ctx: &Context,
    signals: &Signals,
) -> Result<Vec<u8>> {
    signals.check(ctx)?;
    device.check()?;
    let (_, content, _) = geometry.validate()?;
    let surface = win(frame.Surface(), "frame surface")?;
    let access: IDirect3DDxgiInterfaceAccess = win(surface.cast(), "surface DXGI access")?;
    let texture: ID3D11Texture2D = win(unsafe { access.GetInterface() }, "frame texture")?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe {
        texture.GetDesc(&mut desc);
    }
    Size::new(desc.Width as i32, desc.Height as i32)?;
    if desc.Width < content.width
        || desc.Height < content.height
        || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
        || desc.MipLevels != 1
        || desc.ArraySize != 1
        || desc.SampleDesc.Count != 1
    {
        return Err(Error::action(
            "WGC frame texture format, extent or sample count is invalid",
        ));
    }
    let desc = D3D11_TEXTURE2D_DESC {
        Width: content.width,
        Height: content.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    win(
        unsafe {
            device
                .device
                .CreateTexture2D(&desc, None, Some(&mut staging))
        },
        "staging texture",
    )?;
    let staging = staging.ok_or_else(|| Error::action("WGC staging texture missing"))?;
    let region = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: content.width,
        bottom: content.height,
        back: 1,
    };
    unsafe {
        device
            .context
            .CopySubresourceRegion(&staging, 0, 0, 0, 0, &texture, 0, Some(&region));
        device.context.Flush();
    }
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    loop {
        signals.check(ctx)?;
        device.check()?;
        match unsafe {
            device.context.Map(
                &staging,
                0,
                D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                Some(&mut mapped),
            )
        } {
            Ok(()) => break,
            Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                std::thread::sleep(ctx.wait_quantum())
            }
            Err(error) => return win(Err(error), "map staging pixels"),
        }
    }
    let _mapped = Mapped {
        context: &device.context,
        texture: &staging,
    };
    let length = model::mapped_len(content, mapped.RowPitch as usize)?;
    if mapped.pData.is_null() {
        return Err(Error::action("WGC mapped pixels are null"));
    }
    // The validated texture and successful READ Map own this row-pitched range
    // until the guard unmaps it; never use DepthPitch as a 2-D allocation size.
    let data = unsafe { std::slice::from_raw_parts(mapped.pData.cast::<u8>(), length) };
    let rgba = model::bgra_to_window_rgba(geometry, mapped.RowPitch as usize, data)?;
    signals.check(ctx)?;
    device.check()?;
    Ok(rgba)
}
fn capture(root: Root, ctx: &Context) -> Result<Screenshot> {
    ctx.check()?;
    geometry(root)?;
    let interop: IGraphicsCaptureItemInterop = win(
        factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>(),
        "capture factory",
    )?;
    let item: GraphicsCaptureItem = win(
        unsafe { interop.CreateForWindow(HWND(root.hwnd as *mut c_void)) },
        "CreateForWindow",
    )?;
    let size = win(item.Size(), "item size")?;
    let mut frames = Frames::new(Size::new(size.Width, size.Height)?)?;
    let device = Device::new()?;
    ctx.check()?;
    let (mut owner, wake) = Session::new(item, &device, frames.size)?;
    let result = (|| {
        loop {
            owner.signals.check(ctx)?;
            device.check()?;
            let current = geometry(root)?;
            let pool = owner.pool.as_ref().unwrap();
            let Some(mut frame) = next_frame(pool)? else {
                match wake.recv_timeout(ctx.wait_quantum()) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => return Err(Error::action("WGC frame callback disconnected")),
                }
            };
            let process = (|| {
                let value = frame.0.as_ref().unwrap();
                let size = win(value.ContentSize(), "frame content size")?;
                let size = Size::new(size.Width, size.Height)?;
                let time = win(value.SystemRelativeTime(), "frame timestamp")?.Duration;
                match frames.observe(size, time)? {
                    FrameDecision::Discard => Ok(None),
                    FrameDecision::Recreate(size) => Ok(Some(Err(size))),
                    FrameDecision::Accept => {
                        let (_, content, _) = current.validate()?;
                        if size != content {
                            return Ok(None);
                        }
                        let rgba = pixels(&device, value, current, ctx, &owner.signals)?;
                        if geometry(root)? != current {
                            return Ok(None);
                        }
                        let (outer, _, _) = current.validate()?;
                        let png = model::encode_png(outer, &rgba, model::MAX_ENCODED_BYTES)?;
                        owner.signals.check(ctx)?;
                        if geometry(root)? != current {
                            return Ok(None);
                        }
                        Ok(Some(Ok(Screenshot {
                            root,
                            geometry: current,
                            generation: frames.generation,
                            png,
                        })))
                    }
                }
            })();
            let closed = frame.close();
            let processed = match process {
                Err(e) => return Err(with_cleanup(e, closed.err())),
                Ok(value) => {
                    closed?;
                    value
                }
            };
            match processed {
                Some(Ok(shot)) => return Ok(shot),
                Some(Err(size)) => {
                    // No frame/surface reference survives into pool recreation;
                    // WGC discards the old pool's queued generation itself.
                    owner.signals.check(ctx)?;
                    win(
                        pool.Recreate(
                            &device.projected,
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            2,
                            size_int(size),
                        ),
                        "recreate resized pool",
                    )?;
                }
                None => {}
            }
        }
    })();
    let closed = owner.close();
    match result {
        Ok(shot) => {
            closed?;
            ctx.check()?;
            Ok(shot)
        }
        Err(e) => Err(with_cleanup(e, closed.err())),
    }
}
