//! Fresh exact-window captures using ScreenCaptureKit.
//! Every request discovers the window afresh; no retained Cocoa objects or frames
//! cross callback/thread boundaries or survive a capture.
use super::super::screenshot;
use super::*;
pub(super) fn capture_window(
    pid: i32,
    ax_frame: [f64; 4],
    window_id: Option<u32>,
    geometry: screenshot::Geometry,
    encoding: screenshot::Encoding,
) -> Result<Image> {
    let request = super::super::WindowCapture {
        pid,
        frame: ax_frame,
        window_id,
        pixels: geometry.pixels,
        encoding,
    };
    if let Some(result) = super::super::window_capture::delegated_capture(&request) {
        return result;
    }
    let receive = start_capture(request)?;
    let start = Instant::now();
    loop {
        super::super::check_native_cancellation()?;
        match receive.try_recv() {
            Ok(result) => return result,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(Error::action("Screenshot callback disconnected"));
            }
            _ => {}
        }
        if start.elapsed() > Duration::from_secs(10) {
            return Err(Error::action("Screenshot timed out"));
        }
        pump(Duration::from_millis(10));
    }
}
// A callback that macOS never invokes must retain its permit. Repeated timed-out
// requests therefore cannot accumulate unbounded callback allocations.
static INFLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct Permit(std::sync::atomic::AtomicBool);
impl Permit {
    fn complete(&self) {
        // macOS can retain an already-invoked block. Release admission exactly
        // once on completion, independently of that block's eventual lifetime.
        if !self.0.swap(true, std::sync::atomic::Ordering::AcqRel) {
            INFLIGHT.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        }
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.complete();
    }
}
pub fn start_capture(
    request: super::super::WindowCapture,
) -> Result<std::sync::mpsc::Receiver<Result<Image>>> {
    request.validate()?;
    // CLI parents can reach SCK before any AppKit window discovery initializes
    // their WindowServer connection. Initialize AppKit without activation.
    // The direct owned-window test already initializes its connection and runs
    // on the Rust test thread; it retains the original synchronous wrapper.
    if let Some(main) = objc2::MainThreadMarker::new() {
        let _application = objc2_app_kit::NSApplication::sharedApplication(main);
    }
    let super::super::WindowCapture {
        pid,
        frame: ax_frame,
        window_id,
        pixels,
        encoding,
    } = request;
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        return Err(Error::new(
            -32003,
            "Grant Screen Recording permission to this executable or launching terminal",
        ));
    }
    use std::sync::atomic::Ordering;
    INFLIGHT
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < 32).then_some(n + 1)
        })
        .map_err(|_| Error::action("Window capture callback limit reached"))?;
    let permit = std::sync::Arc::new(Permit(std::sync::atomic::AtomicBool::new(false)));
    let (send, receive) = std::sync::mpsc::channel();
    let content_callback = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let permit = permit.clone();
            let fail = |message: String| {
                permit.complete();
                let _ = send.send(Err(Error::action(message)));
            };
            // SAFETY: ScreenCaptureKit guarantees callback pointers for the duration
            // of the invocation. No borrowed Cocoa objects leave this callback.
            unsafe {
                if let Some(error) = error.as_ref() {
                    fail(error.localizedDescription().to_string());
                    return;
                }
                let Some(content) = content.as_ref() else {
                    fail("No shareable content".into());
                    return;
                };
                let windows = content.windows();
                let matches: Vec<_> = windows
                    .iter()
                    .filter(|w| {
                        w.owningApplication().is_some_and(|a| a.processID() == pid)
                            && w.windowLayer() == 0
                            && window_id.is_none_or(|id| w.windowID() == id)
                            && (window_id.is_some() || {
                                let [x, y, width, height] = ax_frame;
                                let f = w.frame();
                                (f.origin.x - x).abs() < 2.
                                    && (f.origin.y - y).abs() < 2.
                                    && (f.size.width - width).abs() < 2.
                                    && (f.size.height - height).abs() < 2.
                            })
                    })
                    .collect();
                // The native AX tree represents one window. Match the known ID,
                // or require a unique frame match when no native ID is available.
                if matches.len() != 1 {
                    fail(window_capture_match_error(matches.len()));
                    return;
                }
                let filter = SCContentFilter::initWithDesktopIndependentWindow(
                    SCContentFilter::alloc(),
                    &matches[0],
                );
                let rect = filter.contentRect();
                let actual_frame = matches[0].frame();
                let actual_frame = [
                    actual_frame.origin.x,
                    actual_frame.origin.y,
                    actual_frame.size.width,
                    actual_frame.size.height,
                ];
                if actual_frame
                    .into_iter()
                    .zip(ax_frame)
                    .any(|(actual, expected)| {
                        !actual.is_finite() || (actual - expected).abs() > 0.01
                    })
                    || (rect.size.width - ax_frame[2]).abs() > 0.01
                    || (rect.size.height - ax_frame[3]).abs() > 0.01
                    || !rect.size.width.is_finite()
                    || !rect.size.height.is_finite()
                {
                    fail("Capture content does not match the observed window geometry".into());
                    return;
                }
                let config = SCStreamConfiguration::new();
                config.setWidth(pixels[0]);
                config.setHeight(pixels[1]);
                config.setShowsCursor(false);
                config.setIgnoreShadowsSingleWindow(true);
                let sender = send.clone();
                let callback = RcBlock::new(
                    move |image: *mut objc2_core_graphics::CGImage, error: *mut NSError| {
                        let result = if let Some(error) = error.as_ref() {
                            Err(Error::action(error.localizedDescription().to_string()))
                        } else if image.is_null() {
                            Err(Error::action("No screenshot image"))
                        } else {
                            screenshot::macos::encode(image as *mut c_void, encoding, pixels)
                        };
                        permit.complete();
                        let _ = sender.send(result);
                    },
                );
                SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                    &filter,
                    &config,
                    Some(&callback),
                );
            }
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(true,false,&content_callback)
    };
    Ok(receive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::process::{Command, Stdio};

    #[test]
    #[ignore = "owned AppKit window; requires Screen Recording and swiftc"]
    fn owned_capture_delivers_fresh_frames_and_rejects_invalid_geometry() {
        let workspace = NSWorkspace::sharedWorkspace();
        let before_front = workspace
            .frontmostApplication()
            .map(|a| a.processIdentifier());
        let before_cursor = CGEvent::new(source().unwrap()).unwrap().location();
        assert!(unsafe { CGPreflightScreenCaptureAccess() });
        let dir = tempfile::tempdir().unwrap();
        let swift_source = dir.path().join("capture.swift");
        let executable = dir.path().join("CaptureFixture");
        let command = dir.path().join("command");
        let receipt = dir.path().join("receipt");
        std::fs::write(&swift_source, r#"
import AppKit
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let command = CommandLine.arguments[1], receipt = CommandLine.arguments[2]
let window = NSWindow(contentRect: NSRect(x:80,y:80,width:360,height:240), styleMask:[.titled], backing:.buffered, defer:false)
window.title = "Nanocodex owned screenshot benchmark"
window.isReleasedWhenClosed = false
let view = NSView(frame:NSRect(x:0,y:0,width:360,height:240))
view.wantsLayer = true
window.contentView = view
window.orderBack(nil)
var previous = ""
Timer.scheduledTimer(withTimeInterval:0.01, repeats:true) { _ in
    guard let value = try? String(contentsOfFile:command, encoding:.utf8), value != previous else { return }
    previous = value
    if value == "close" { window.close() }
    else if value == "move" { window.setFrameOrigin(NSPoint(x:120,y:80)) }
    else { view.layer?.backgroundColor = (value.hasPrefix("red") ? NSColor.red : NSColor.blue).cgColor; view.displayIfNeeded() }
    let f = window.frame
    let h = NSScreen.screens[0].frame.height
    try! "\(value) \(ProcessInfo.processInfo.processIdentifier) \(window.windowNumber) \(f.origin.x) \(h-f.maxY) \(f.width) \(f.height)".write(toFile:receipt, atomically:true, encoding:.utf8)
}
Timer.scheduledTimer(withTimeInterval:45, repeats:false) { _ in app.terminate(nil) }
app.run()
"#).unwrap();
        assert!(
            Command::new("swiftc")
                .arg(&swift_source)
                .arg("-o")
                .arg(&executable)
                .status()
                .unwrap()
                .success()
        );
        let child = Command::new(&executable)
            .arg(&command)
            .arg(&receipt)
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        struct Stop(std::process::Child);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut stop = Stop(child);
        let control = |value: &str| {
            std::fs::write(&command, value).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(text) = std::fs::read_to_string(&receipt) {
                    if text.split_whitespace().next() == Some(value) {
                        // Let AppKit commit the requested color to WindowServer.
                        pump(Duration::from_millis(50));
                        return text
                            .split_whitespace()
                            .skip(1)
                            .map(|s| s.parse::<f64>().unwrap())
                            .collect::<Vec<_>>();
                    }
                }
                assert!(
                    Instant::now() < deadline,
                    "fixture did not acknowledge {value}"
                );
                pump(Duration::from_millis(10));
            }
        };
        let v = control("blue-start");
        let pid = v[0] as i32;
        let id = v[1] as u32;
        let frame = [v[2], v[3], v[4], v[5]];
        let configuration = screenshot::Configuration::default();
        let geometry = screenshot::Geometry::new(frame, 1., configuration).unwrap();
        let mut durations = Vec::new();
        for round in 0..12 {
            let red = round % 2 == 0;
            control(&format!("{}-{round}", if red { "red" } else { "blue" }));
            let started = Instant::now();
            let image =
                capture_window(pid, frame, Some(id), geometry, configuration.encoding).unwrap();
            let elapsed = started.elapsed().as_secs_f64() * 1000.;
            durations.push(elapsed);
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(image.data)
                .unwrap();
            let pixels = image::load_from_memory(&bytes).unwrap().to_rgb8();
            let p = pixels.get_pixel(pixels.width() / 2, pixels.height() / 2);
            assert!(
                if red {
                    p[0] > 200 && p[2] < 50
                } else {
                    p[2] > 200 && p[0] < 50
                },
                "fresh owned content required: {p:?}"
            );
            eprintln!("capture fresh frame round={round} elapsed_ms={elapsed:.2} pixel={p:?}");
        }
        assert_eq!(
            workspace
                .frontmostApplication()
                .map(|a| a.processIdentifier()),
            before_front
        );
        assert_eq!(
            {
                let p = CGEvent::new(source().unwrap()).unwrap().location();
                [p.x, p.y]
            },
            [before_cursor.x, before_cursor.y]
        );
        let mut warm = durations[1..].to_vec();
        warm.sort_by(f64::total_cmp);
        eprintln!(
            "capture summary cold_ms={:.2} warm_median_ms={:.2}",
            durations[0],
            warm[warm.len() / 2]
        );
        assert!(
            capture_window(
                pid + 100000,
                frame,
                Some(id),
                geometry,
                configuration.encoding
            )
            .is_err()
        );
        control("move");
        assert!(
            capture_window(pid, frame, Some(id), geometry, configuration.encoding).is_err(),
            "moved window must reject old geometry"
        );
        let v = control("blue-moved");
        let moved = [v[2], v[3], v[4], v[5]];
        let geometry = screenshot::Geometry::new(moved, 1., configuration).unwrap();
        capture_window(pid, moved, Some(id), geometry, configuration.encoding).unwrap();
        control("close");
        // A retained closed NSWindow can linger in ScreenCaptureKit. Public
        // capture rejects it through AX; here require the owner to have exited.
        stop.0.kill().unwrap();
        stop.0.wait().unwrap();
        assert!(
            capture_window(pid, moved, Some(id), geometry, configuration.encoding).is_err(),
            "closed window must fail"
        );
    }
}
