//! Pure WGC pixel/generation contracts and synthetic owner-thread boundary tests.
//! No capture, COM, GPU, screen or window API is executed by this file.
use skyre::{
    Error, Result,
    platforms::{windows_capture_model::*, windows_uia_model::Root},
};
use std::{
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
fn root() -> Root {
    Root { hwnd: 900, pid: 71 }
}
fn geometry() -> Geometry {
    Geometry {
        window: [-40, 20, 4, 4],
        content: [-39, 21, 2, 2],
    }
}
fn png() -> Vec<u8> {
    encode_png(Size::new(1, 1).unwrap(), &[1, 2, 3, 255], 1024).unwrap()
}
#[test]
fn wgc_rows_exclude_texture_padding_and_preserve_window_pixel_origin() {
    let input = [
        30, 20, 10, 255, 60, 50, 40, 128, 0xde, 0xad, 0xbe, 0xef, 90, 80, 70, 255, 30, 20, 10, 0,
    ];
    assert_eq!(mapped_len(Size::new(2, 2).unwrap(), 12).unwrap(), 20);
    let rgba = bgra_to_window_rgba(geometry(), 12, &input).unwrap();
    assert_eq!(rgba.len(), 64);
    let pixel = |x: usize, y: usize| &rgba[(y * 4 + x) * 4..(y * 4 + x + 1) * 4];
    assert_eq!(pixel(1, 1), [10, 20, 30, 255]);
    assert_eq!(pixel(2, 1), [40, 50, 60, 128]);
    assert_eq!(pixel(1, 2), [70, 80, 90, 255]);
    assert_eq!(pixel(2, 2), [0, 0, 0, 0]);
    for y in 0..4 {
        for x in 0..4 {
            if x == 0 || x == 3 || y == 0 || y == 3 {
                assert_eq!(pixel(x, y), [0, 0, 0, 0]);
            }
        }
    }
    assert_eq!(geometry().frame(), [-40., 20., 4., 4.]);
}
#[test]
fn wgc_rejects_invalid_extents_pitch_truncation_and_oversized_allocations() {
    for (w, h) in [(0, 1), (1, 0), (-1, 2), (i32::MAX, 1), (16384, 16384)] {
        assert!(Size::new(w, h).is_err());
    }
    let size = Size::new(2, 2).unwrap();
    assert!(mapped_len(size, 7).is_err());
    assert!(mapped_len(size, usize::MAX).is_err());
    assert!(bgra_to_window_rgba(geometry(), 12, &[0; 19]).is_err());
    for content in [
        [-41, 21, 2, 2],
        [-39, 19, 2, 2],
        [-38, 21, 3, 2],
        [-39, 22, 2, 3],
    ] {
        assert!(
            Geometry {
                content,
                ..geometry()
            }
            .validate()
            .is_err()
        );
    }
}
#[test]
fn wgc_png_roundtrip_retains_rgba_and_stops_at_encoded_output_bound() {
    let geometry = geometry();
    let rgba = bgra_to_window_rgba(
        geometry,
        8,
        &[
            0, 0, 255, 255, 0, 255, 0, 255, 255, 0, 0, 255, 255, 255, 255, 255,
        ],
    )
    .unwrap();
    let encoded = encode_png(Size::new(4, 4).unwrap(), &rgba, 1024).unwrap();
    let decoded = image::load_from_memory(&encoded).unwrap().to_rgba8();
    assert_eq!((decoded.width(), decoded.height()), (4, 4));
    assert_eq!(decoded.as_raw(), &rgba);
    assert!(
        encode_png(Size::new(4, 4).unwrap(), &rgba, 16)
            .unwrap_err()
            .message
            .contains("output bound")
    );
    assert!(encode_png(Size::new(4, 4).unwrap(), &rgba[..63], 1024).is_err());
}
#[test]
fn wgc_resize_generations_discard_older_frames_and_bound_unstable_windows() {
    let first = Size::new(10, 10).unwrap();
    let second = Size::new(20, 20).unwrap();
    let mut frames = Frames::new(first).unwrap();
    assert_eq!(frames.observe(first, 10).unwrap(), FrameDecision::Accept);
    assert_eq!(frames.observe(first, 10).unwrap(), FrameDecision::Discard);
    assert_eq!(
        frames.observe(second, 11).unwrap(),
        FrameDecision::Recreate(second)
    );
    assert_eq!(frames.generation, 1);
    assert_eq!(frames.observe(first, 10).unwrap(), FrameDecision::Discard);
    assert_eq!(frames.size, second);
    assert_eq!(frames.observe(second, 12).unwrap(), FrameDecision::Accept);
    for n in 2..=MAX_RESIZES {
        let size = Size::new(20 + n as i32, 20).unwrap();
        assert_eq!(
            frames.observe(size, 12 + n as i64).unwrap(),
            FrameDecision::Recreate(size)
        );
    }
    assert!(
        frames
            .observe(first, 100)
            .unwrap_err()
            .message
            .contains("resize bound")
    );
    assert!(frames.observe(first, -1).is_err());
}

#[test]
fn wgc_screenshot_binding_rejects_owner_and_content_geometry_changes() {
    let binding = Binding {
        root: root(),
        geometry: geometry(),
        generation: 3,
    };
    assert_eq!(binding.generation, 3);
    binding.validate(root(), geometry()).unwrap();
    for target in [
        Root {
            hwnd: 901,
            ..root()
        },
        Root { pid: 72, ..root() },
    ] {
        assert!(binding.validate(target, geometry()).is_err());
    }
    let mut changed = geometry();
    changed.content[0] += 1;
    assert!(binding.validate(root(), changed).is_err());
    changed = geometry();
    changed.window[0] -= 1;
    assert!(binding.validate(root(), changed).is_err());
}
struct Fake {
    _not_send: Rc<()>,
    trace: Arc<Mutex<Vec<(&'static str, thread::ThreadId)>>>,
    done: mpsc::Sender<()>,
    release: Option<mpsc::Receiver<()>>,
    error: Option<Error>,
    wrong_root: bool,
}
impl Provider for Fake {
    fn capture(&mut self, target: Root, ctx: &Context) -> Result<Screenshot> {
        assert_eq!(target, root());
        self.trace
            .lock()
            .unwrap()
            .push(("capture", thread::current().id()));
        if let Some(release) = self.release.take() {
            release.recv().unwrap();
        }
        ctx.check()?;
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        Ok(Screenshot {
            root: if self.wrong_root {
                Root { pid: 72, ..target }
            } else {
                target
            },
            geometry: geometry(),
            generation: 0,
            png: png(),
        })
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.trace
            .lock()
            .unwrap()
            .push(("drop", thread::current().id()));
        let _ = self.done.send(());
    }
}
#[test]
fn wgc_worker_owns_non_send_provider_and_rejects_wrong_target_without_success() {
    let trace = Arc::new(Mutex::new(vec![]));
    let check = trace.clone();
    let (done, finished) = mpsc::channel();
    let mut worker = Worker::spawn(
        move || {
            trace.lock().unwrap().push(("init", thread::current().id()));
            Ok(Box::new(Fake {
                _not_send: Rc::new(()),
                trace,
                done,
                release: None,
                error: None,
                wrong_root: true,
            }))
        },
        Duration::from_secs(2),
    )
    .unwrap();
    assert!(
        worker
            .capture(root())
            .unwrap_err()
            .message
            .contains("invalid target")
    );
    drop(worker);
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
    let trace = check.lock().unwrap();
    assert_eq!(
        trace.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        ["init", "capture", "drop"]
    );
    assert_ne!(trace[0].1, thread::current().id());
    assert!(trace.iter().all(|(_, id)| *id == trace[0].1));
}
#[test]
fn wgc_host_timeout_suppresses_late_pixels_and_disables_reuse() {
    let trace = Arc::new(Mutex::new(vec![]));
    let (done, finished) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let mut worker = Worker::spawn(
        move || {
            Ok(Box::new(Fake {
                _not_send: Rc::new(()),
                trace,
                done,
                release: Some(wait),
                error: None,
                wrong_root: false,
            }))
        },
        Duration::from_millis(100),
    )
    .unwrap();
    assert_eq!(worker.capture(root()).unwrap_err().code, -32008);
    assert!(
        worker
            .capture(root())
            .unwrap_err()
            .message
            .contains("unavailable")
    );
    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(2)).unwrap();
}
#[test]
fn wgc_returned_timeout_and_native_failures_preserve_distinct_worker_lifetimes() {
    for code in [-32008, -10005] {
        let trace = Arc::new(Mutex::new(vec![]));
        let (done, finished) = mpsc::channel();
        let mut worker = Worker::spawn(
            move || {
                Ok(Box::new(Fake {
                    _not_send: Rc::new(()),
                    trace,
                    done,
                    release: None,
                    error: Some(Error::new(code, "owned capture failure")),
                    wrong_root: false,
                }))
            },
            Duration::from_secs(2),
        )
        .unwrap();
        let error = worker.capture(root()).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(error.message, "owned capture failure");
        let again = worker.capture(root()).unwrap_err();
        assert_eq!(
            again.message,
            if code == -32008 {
                "WGC worker unavailable after timeout or shutdown"
            } else {
                "owned capture failure"
            }
        );
        drop(worker);
        finished.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    let error = Worker::spawn(
        || Err(Error::new(-10005, "owned device initialization failed")),
        Duration::from_secs(2),
    )
    .err()
    .unwrap();
    assert_eq!(error.message, "owned device initialization failed");
}
