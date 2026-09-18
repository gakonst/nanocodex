//! Scalar-only request crossing the private window-worker pipe.
use super::{Image, screenshot::Encoding};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowCapture {
    pub pid: i32,
    pub window_id: Option<u32>,
    pub frame: [f64; 4],
    pub pixels: [usize; 2],
    pub encoding: Encoding,
}
impl WindowCapture {
    pub fn validate(&self) -> Result<()> {
        if self.pid <= 0
            || !self.frame.into_iter().all(f64::is_finite)
            || self.frame[2] <= 0.
            || self.frame[3] <= 0.
            || self.pixels.iter().any(|n| !(1..=2048).contains(n))
            || self.pixels[0] * self.pixels[1] > 2048 * 768
            || matches!(self.encoding, Encoding::Jpeg { quality: Some(q) } if !q.is_finite() || q <= 0. || q > 1.)
        {
            return Err(Error::invalid("Invalid window capture request"));
        }
        Ok(())
    }
}
type Delegate = Box<dyn Fn(WindowCapture) -> Result<Image>>;
thread_local! {
    static DELEGATE: std::cell::RefCell<Option<Delegate>> = const { std::cell::RefCell::new(None) };
}
pub fn set_window_capture_delegate(delegate: Option<Delegate>) {
    #[cfg(target_os = "macos")]
    if delegate.is_some() {
        if let Some(main) = objc2::MainThreadMarker::new() {
            // The child still owns AX and NSRunningApplication on its main
            // thread even though all SCK work is delegated to the parent.
            let _application = objc2_app_kit::NSApplication::sharedApplication(main);
        }
    }
    DELEGATE.with(|slot| *slot.borrow_mut() = delegate);
}
#[cfg(target_os = "macos")]
pub(super) fn delegated_capture(request: &WindowCapture) -> Option<Result<Image>> {
    DELEGATE.with(|slot| slot.borrow().as_ref().map(|call| call(request.clone())))
}
pub fn start_window_capture(
    request: WindowCapture,
) -> Result<std::sync::mpsc::Receiver<Result<Image>>> {
    request.validate()?;
    #[cfg(target_os = "macos")]
    {
        super::macos::start_window_capture(request)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(Error::unsupported("Window capture broker requires macOS"))
    }
}
