//! Visual feedback only. All AppKit ownership is main-thread confined; workers
//! enqueue scalar updates. The host must service the main CFRunLoop while idle.
//! Panels are ordered directly above their foreign target, never floating above
//! the human's foreground windows. WindowServer ordering is best effort (Spaces
//! and foreign applications reordering their windows can hide the cursor).
use block2::RcBlock;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSColor, NSPanel, NSView,
    NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSTimer};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::c_void,
    time::Instant,
};

const HOLD: f64 = 0.85;
const FADE: f64 = 0.35;
const SIZE: f64 = 56.;
const TIP: [f64; 2] = [17., 14.];
const MAX_WINDOWS: usize = 128;

#[derive(Clone, Copy)]
pub(super) struct Update {
    pub pid: i32,
    pub window: u32,
    pub frame: [f64; 4],
    pub point: [f64; 2],
    pub click: bool,
}

fn opacity(age: f64) -> f64 {
    (1. - (age - HOLD).max(0.) / FADE).clamp(0., 1.)
}
fn ease(t: f64) -> f64 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}
fn interpolate(from: [f64; 2], to: [f64; 2], elapsed: f64) -> [f64; 2] {
    let t = ease(elapsed / 0.075);
    [
        from[0] + (to[0] - from[0]) * t,
        from[1] + (to[1] - from[1]) * t,
    ]
}
// Clip the actual panel to the target's bounds, including the titlebar. Points
// are WindowServer global coordinates (top-left primary display origin).
fn clipped_rect(point: [f64; 2], frame: [f64; 4]) -> [f64; 4] {
    let x = (point[0] - TIP[0]).max(frame[0]);
    let y = (point[1] - TIP[1]).max(frame[1]);
    let right = (point[0] - TIP[0] + SIZE).min(frame[0] + frame[2]);
    let bottom = (point[1] - TIP[1] + SIZE).min(frame[1] + frame[3]);
    [x, y, (right - x).max(0.), (bottom - y).max(0.)]
}

#[derive(Default)]
struct Drawing {
    tip: Cell<[f64; 2]>,
    pulse: Cell<f64>,
    hue: Cell<f64>,
}
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Drawing]
    struct CursorView;
    impl CursorView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool { true }
        #[unsafe(method(drawRect:))]
        fn draw(&self, _rect: NSRect) {
            let [x, y] = self.ivars().tip.get();
            let hue = self.ivars().hue.get();
            let pulse = self.ivars().pulse.get();
            if pulse < 1. {
                let r = 5. + 17. * ease(pulse);
                NSColor::colorWithCalibratedHue_saturation_brightness_alpha(hue, 0.65, 1., (1. - pulse) * 0.75).setStroke();
                let ring = NSBezierPath::bezierPathWithOvalInRect(NSRect::new(NSPoint::new(x-r,y-r), NSSize::new(r*2.,r*2.)));
                ring.setLineWidth(2.5);
                ring.stroke();
            }
            let path = NSBezierPath::bezierPath();
            path.moveToPoint(NSPoint::new(x, y));
            for [dx,dy] in [[2.,25.],[8.,18.],[13.,29.],[18.,26.],[13.,16.],[22.,15.]] {
                path.lineToPoint(NSPoint::new(x+dx,y+dy));
            }
            path.closePath();
            NSColor::colorWithCalibratedWhite_alpha(0., 0.22).setStroke();
            path.setLineWidth(5.);
            path.stroke();
            NSColor::whiteColor().setStroke();
            path.setLineWidth(3.);
            path.stroke();
            NSColor::colorWithCalibratedHue_saturation_brightness_alpha(hue, 0.72, 0.95, 1.).setFill();
            path.fill();
        }
    }
);

define_class!(
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    struct CursorPanel;
    impl CursorPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool { false }
        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool { false }
    }
);

struct Cursor {
    panel: Retained<CursorPanel>,
    view: Retained<CursorView>,
    update: Update,
    from: [f64; 2],
    shown: [f64; 2],
    moved: Instant,
    touched: Instant,
    clicked: Option<Instant>,
}
impl Drop for Cursor {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}
#[derive(Default)]
struct Cursors {
    windows: BTreeMap<(i32, u32), Cursor>,
    timer: Option<Retained<NSTimer>>,
    frames: BTreeMap<(i32, u32), [f64; 4]>,
    sampled: Option<Instant>,
}
thread_local! { static CURSORS: RefCell<Cursors> = RefCell::new(Cursors::default()); }

unsafe extern "C" {
    static _dispatch_main_q: c_void;
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: unsafe extern "C" fn(*mut c_void),
    );
}
#[derive(Default)]
struct Pending {
    scheduled: bool,
    remove: std::collections::BTreeSet<(i32, u32)>,
    updates: BTreeMap<(i32, u32), Update>,
}
static PENDING: std::sync::Mutex<Pending> = std::sync::Mutex::new(Pending {
    scheduled: false,
    remove: std::collections::BTreeSet::new(),
    updates: BTreeMap::new(),
});
unsafe extern "C" fn receive(_: *mut c_void) {
    let (remove, updates) = {
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        pending.scheduled = false;
        (
            std::mem::take(&mut pending.remove),
            std::mem::take(&mut pending.updates),
        )
    };
    objc2::rc::autoreleasepool(|_| {
        if let Some(mtm) = MainThreadMarker::new() {
            CURSORS.with_borrow_mut(|s| {
                for key in remove {
                    s.windows.remove(&key);
                }
                if s.windows.is_empty() {
                    if let Some(timer) = s.timer.take() {
                        timer.invalidate();
                    }
                }
            });
            for update in updates.into_values() {
                apply(mtm, update);
            }
        }
    });
}
/// Clear only targets whose last session owner has gone away.
pub(super) fn clear_targets(targets: Vec<(i32, u32)>) {
    let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    for key in targets {
        pending.updates.remove(&key);
        pending.remove.insert(key);
    }
    if MainThreadMarker::new().is_some() {
        drop(pending);
        unsafe {
            receive(std::ptr::null_mut());
        }
    } else if !pending.scheduled {
        pending.scheduled = true;
        unsafe {
            dispatch_async_f(
                std::ptr::addr_of!(_dispatch_main_q),
                std::ptr::null_mut(),
                receive,
            );
        }
    }
}
pub(super) fn post(mut update: Update) {
    if let Some(mtm) = MainThreadMarker::new() {
        // A newer main-thread action supersedes older queued worker updates and
        // teardown for this target. Never let an old dispatch hide a new cursor.
        {
            let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
            pending.remove.remove(&(update.pid, update.window));
            pending.updates.remove(&(update.pid, update.window));
        }
        apply(mtm, update);
    } else {
        // Coalesce movement without losing a down pulse. A stalled main loop
        // cannot grow an unbounded dispatch queue. AppKit never crosses threads.
        let mut pending = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        let key = (update.pid, update.window);
        pending.remove.remove(&key);
        if let Some(old) = pending.updates.get(&key) {
            update.click |= old.click;
        }
        if pending.updates.len() >= MAX_WINDOWS && !pending.updates.contains_key(&key) {
            pending.updates.pop_first();
        }
        pending.updates.insert(key, update);
        if !pending.scheduled {
            pending.scheduled = true;
            unsafe {
                dispatch_async_f(
                    std::ptr::addr_of!(_dispatch_main_q),
                    std::ptr::null_mut(),
                    receive,
                );
            }
        }
    }
}

fn apply(mtm: MainThreadMarker, update: Update) {
    objc2::rc::autoreleasepool(|_| apply_inner(mtm, update));
}
fn apply_inner(mtm: MainThreadMarker, update: Update) {
    CURSORS.with_borrow_mut(|state| {
        let now = Instant::now();
        let key = (update.pid, update.window);
        if !state.windows.contains_key(&key) {
            if state.windows.len() >= MAX_WINDOWS {
                if let Some(oldest) = state.windows.iter().min_by_key(|(_, c)| c.touched).map(|(k,_)| *k) { state.windows.remove(&oldest); }
            }
            // Creating a shared application does not activate it or change its
            // activation policy. The embedding host owns application lifecycle.
            let _ = NSApplication::sharedApplication(mtm);
            let panel: Retained<CursorPanel> = unsafe { msg_send![super(CursorPanel::alloc(mtm).set_ivars(())), initWithContentRect: NSRect::new(NSPoint::new(0.,0.), NSSize::new(SIZE,SIZE)), styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, backing: NSBackingStoreType::Buffered, defer: false] };
            unsafe { panel.setReleasedWhenClosed(false); }
            panel.setIgnoresMouseEvents(true);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setHasShadow(false);
            panel.setHidesOnDeactivate(false);
            panel.setFloatingPanel(false);
            panel.setLevel(0);
            panel.setBecomesKeyOnlyIfNeeded(true);
            panel.setCollectionBehavior(NSWindowCollectionBehavior::Transient | NSWindowCollectionBehavior::IgnoresCycle);
            let view: Retained<CursorView> = unsafe { msg_send![super(CursorView::alloc(mtm).set_ivars(Drawing::default())), initWithFrame: NSRect::new(NSPoint::new(0.,0.), NSSize::new(SIZE,SIZE))] };
            view.ivars().hue.set(((update.window.wrapping_mul(137) % 360) as f64) / 360.);
            panel.setContentView(Some(&view));
            state.windows.insert(key, Cursor { panel, view, update, from:update.point, shown:update.point, moved:now, touched:now, clicked:None });
        }
        let cursor = state.windows.get_mut(&key).unwrap();
        cursor.from = cursor.shown;
        cursor.moved = now;
        cursor.touched = now;
        cursor.update = update;
        if update.click { cursor.clicked = Some(now); }
        if state.timer.is_none() {
            let block = RcBlock::new(|_: std::ptr::NonNull<NSTimer>| { tick(); });
            state.timer = Some(unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1./60., true, &block) });
        }
    });
    tick();
}
// One WindowServer snapshot per frame for all cursors. Metadata contains no
// captured pixels. Fail closed if the target disappears, minimizes or changes
// Spaces; verify PID too, because window numbers can be reused.
fn visible_frames() -> BTreeMap<(i32, u32), [f64; 4]> {
    use core_foundation::{
        array::CFArray,
        base::{CFType, TCFType},
        dictionary::CFDictionary,
        number::CFNumber,
        string::CFString,
    };
    use core_graphics::window::{copy_window_info, kCGWindowListOptionOnScreenOnly};
    let mut frames = BTreeMap::new();
    let Some(raw) = copy_window_info(kCGWindowListOptionOnScreenOnly, 0) else {
        return frames;
    };
    let list = unsafe {
        CFArray::<CFDictionary<CFString, CFType>>::wrap_under_get_rule(raw.as_concrete_TypeRef())
    };
    let number = |d: &CFDictionary<CFString, CFType>, name: &str| {
        d.find(CFString::new(name))
            .and_then(|v| v.downcast::<CFNumber>())
            .and_then(|v| v.to_f64())
    };
    for d in list.iter() {
        let Some(pid) = number(&d, "kCGWindowOwnerPID") else {
            continue;
        };
        let Some(id) = number(&d, "kCGWindowNumber") else {
            continue;
        };
        let Some(bounds) = d
            .find(CFString::new("kCGWindowBounds"))
            .and_then(|v| v.downcast::<CFDictionary>())
        else {
            continue;
        };
        let bounds = unsafe {
            CFDictionary::<CFString, CFType>::wrap_under_get_rule(bounds.as_concrete_TypeRef())
        };
        let (Some(x), Some(y), Some(w), Some(h)) = (
            number(&bounds, "X"),
            number(&bounds, "Y"),
            number(&bounds, "Width"),
            number(&bounds, "Height"),
        ) else {
            continue;
        };
        if [x, y, w, h].into_iter().all(f64::is_finite) && w > 0. && h > 0. {
            frames.insert((pid as i32, id as u32), [x, y, w, h]);
        }
    }
    frames
}
fn tick() {
    objc2::rc::autoreleasepool(|_| tick_inner());
}
fn tick_inner() {
    let Some(_mtm) = MainThreadMarker::new() else {
        return;
    };
    CURSORS.with_borrow_mut(|state| {
        let now = Instant::now();
        state
            .windows
            .retain(|_, c| opacity(now.duration_since(c.touched).as_secs_f64()) > 0.);
        // Primary-display height, not the menu-bar screen: CG coordinates are
        // defined against the primary display even on negative-origin monitors.
        let primary_height = core_graphics::display::CGDisplay::main()
            .bounds()
            .size
            .height;
        if state
            .sampled
            .is_none_or(|last| now.duration_since(last).as_secs_f64() >= 1. / 30.)
        {
            state.frames = visible_frames();
            state.sampled = Some(now);
        }
        let frames = &state.frames;
        for (key, c) in &mut state.windows {
            let Some(frame) = frames.get(key).copied() else {
                c.panel.orderOut(None);
                continue;
            };
            let delta = [frame[0] - c.update.frame[0], frame[1] - c.update.frame[1]];
            for axis in 0..2 {
                c.from[axis] += delta[axis];
                c.update.point[axis] += delta[axis];
            }
            c.update.frame = frame;
            c.shown = interpolate(
                c.from,
                c.update.point,
                now.duration_since(c.moved).as_secs_f64(),
            );
            let [x, y, w, h] = clipped_rect(c.shown, c.update.frame);
            if w <= 0. || h <= 0. {
                c.panel.orderOut(None);
                continue;
            }
            c.view.ivars().tip.set([c.shown[0] - x, c.shown[1] - y]);
            c.view.ivars().pulse.set(
                c.clicked
                    .map_or(1., |t| (now.duration_since(t).as_secs_f64() / 0.32).min(1.)),
            );
            c.panel.setFrame_display(
                NSRect::new(NSPoint::new(x, primary_height - y - h), NSSize::new(w, h)),
                false,
            );
            c.panel
                .setAlphaValue(opacity(now.duration_since(c.touched).as_secs_f64()));
            c.view.setNeedsDisplay(true);
            c.panel
                .orderWindow_relativeTo(NSWindowOrderingMode::Above, c.update.window as isize);
            c.panel.displayIfNeeded();
        }
        if state.windows.is_empty() {
            if let Some(timer) = state.timer.take() {
                timer.invalidate();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fade_and_motion_have_bounded_endpoints() {
        assert_eq!(opacity(0.), 1.);
        assert_eq!(opacity(HOLD), 1.);
        assert!((opacity(HOLD + FADE / 2.) - 0.5).abs() < 1e-9);
        assert_eq!(opacity(10.), 0.);
        assert_eq!(interpolate([0., 10.], [80., -10.], 0.), [0., 10.]);
        assert_eq!(interpolate([0., 10.], [80., -10.], 1.), [80., -10.]);
        assert_eq!(interpolate([0., 10.], [80., -10.], 0.0375), [40., 0.]);
    }
    #[test]
    fn cursor_panel_clips_to_window_on_negative_origin_display() {
        let frame = [-900., -100., 800., 600.];
        for p in [[-900., -100.], [-101., 499.], [-500., 200.]] {
            let [x, y, w, h] = clipped_rect(p, frame);
            assert!(x >= frame[0] && y >= frame[1]);
            assert!(x + w <= frame[0] + frame[2] && y + h <= frame[1] + frame[3]);
            assert!(w > 0. && h > 0. && w <= SIZE && h <= SIZE);
            assert!(p[0] >= x && p[0] < x + w && p[1] >= y && p[1] < y + h);
        }
    }
}
