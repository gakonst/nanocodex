//! Observer state is confined to its creating CFRunLoop. Drop detaches the source
//! before releasing callback context. Subscription failures remain observable.
use super::{Ax, ax_ok};
use crate::{Error, Result};
use accessibility_sys::*;
use core_foundation::{
    base::{CFType, TCFType},
    runloop::{CFRunLoop, CFRunLoopSource, kCFRunLoopDefaultMode},
    string::CFString,
};
use std::{
    ffi::c_void,
    ptr,
    time::{Duration, Instant},
};
struct State {
    epoch: u64,
    last_change: Option<Instant>,
    layout_changed: bool,
    destroyed: Vec<Ax>,
    menu: Option<Ax>,
    callback_failed: bool,
}
pub struct Monitor {
    observer: CFType,
    root: Ax,
    state: Box<State>,
    registered: Vec<CFString>,
    source: CFRunLoopSource,
    run_loop: CFRunLoop,
    pub failures: Vec<String>,
}
unsafe extern "C" fn callback(
    _: AXObserverRef,
    element: AXUIElementRef,
    name: core_foundation::string::CFStringRef,
    context: *mut c_void,
) {
    if context.is_null() {
        return;
    }
    if name.is_null() || element.is_null() {
        unsafe { (*(context as *mut State)).callback_failed = true };
        return;
    }
    let delivered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let state = &mut *(context as *mut State);
        let name = CFString::wrap_under_get_rule(name).to_string();
        state.epoch = state.epoch.saturating_add(1);
        state.last_change = Some(Instant::now());
        if name == "AXLayoutChanged" {
            state.layout_changed = true;
        }
        let ax = Ax(CFType::wrap_under_get_rule(element as _));
        if name == "AXUIElementDestroyed" {
            state.destroyed.push(ax.clone());
            if state.destroyed.len() > 5000 {
                state.destroyed.remove(0);
                state.layout_changed = true;
            }
        }
        update_open_menu(&mut state.menu, &name, ax);
    }));
    if delivered.is_err() {
        unsafe { (*(context as *mut State)).callback_failed = true };
    }
}
fn update_open_menu<T>(menu: &mut Option<T>, notification: &str, element: T) {
    match notification {
        "AXMenuOpened" => *menu = Some(element),
        "AXMenuClosed" => *menu = None,
        // A shortcut invokes an item without opening a menu. Selection inside
        // an open menu likewise must not replace its root with one item.
        _ => (),
    }
}
impl Monitor {
    pub fn new(pid: i32, root: Ax) -> Result<Self> {
        let mut raw = ptr::null_mut();
        ax_ok(unsafe { AXObserverCreate(pid, callback, &mut raw) })?;
        if raw.is_null() {
            return Err(Error::action("AXObserverCreate returned no observer"));
        }
        let observer = unsafe { CFType::wrap_under_create_rule(raw as _) };
        let raw_source = unsafe { AXObserverGetRunLoopSource(raw) };
        if raw_source.is_null() {
            return Err(Error::action("AX observer has no run loop source"));
        }
        let source = unsafe { CFRunLoopSource::wrap_under_get_rule(raw_source) };
        let run_loop = CFRunLoop::get_current();
        let mut state = Box::new(State {
            epoch: 0,
            last_change: None,
            layout_changed: false,
            destroyed: vec![],
            menu: None,
            callback_failed: false,
        });
        let mut registered = vec![];
        let mut failures = vec![];
        for name in [
            "AXLayoutChanged",
            "AXUIElementDestroyed",
            "AXValueChanged",
            "AXSelectedTextChanged",
            "AXFocusedUIElementChanged",
            "AXFocusedWindowChanged",
            "AXWindowCreated",
            "AXWindowMoved",
            "AXWindowResized",
            "AXMenuOpened",
            "AXMenuClosed",
            "AXMenuItemSelected",
        ] {
            let notification = CFString::new(name);
            let code = unsafe {
                AXObserverAddNotification(
                    raw,
                    root.ptr(),
                    notification.as_concrete_TypeRef(),
                    &mut *state as *mut State as _,
                )
            };
            if code == 0 || code == kAXErrorNotificationAlreadyRegistered {
                registered.push(notification);
            } else {
                failures.push(format!("{name}: {code}"));
            }
        }
        unsafe {
            run_loop.add_source(&source, kCFRunLoopDefaultMode);
        }
        Ok(Self {
            observer,
            root,
            state,
            registered,
            source,
            run_loop,
            failures,
        })
    }
    pub fn epoch(&self) -> u64 {
        self.state.epoch
    }
    pub fn check(&self) -> Result<()> {
        if self.state.callback_failed {
            Err(Error::action(
                "AX observer callback failed; start a new session",
            ))
        } else {
            Ok(())
        }
    }
    pub fn menu(&self) -> Option<Ax> {
        self.state.menu.clone()
    }
    pub fn quiet(&self, now: Instant, delay: Duration, since: Instant) -> bool {
        now.saturating_duration_since(self.state.last_change.unwrap_or(since).max(since)) >= delay
    }
    pub fn acknowledge(&mut self) {
        self.state.layout_changed = false;
        self.state.destroyed.clear();
    }
}
impl Drop for Monitor {
    fn drop(&mut self) {
        unsafe {
            self.run_loop
                .remove_source(&self.source, kCFRunLoopDefaultMode);
            for name in &self.registered {
                AXObserverRemoveNotification(
                    self.observer.as_CFTypeRef() as _,
                    self.root.ptr(),
                    name.as_concrete_TypeRef(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_shortcut_selection_does_not_create_or_replace_open_menu() {
        let mut open = None;
        update_open_menu(&mut open, "AXMenuItemSelected", 99);
        assert_eq!(open, None);
        update_open_menu(&mut open, "AXMenuOpened", 10);
        update_open_menu(&mut open, "AXMenuItemSelected", 11);
        assert_eq!(open, Some(10));
        update_open_menu(&mut open, "AXMenuClosed", 10);
        assert_eq!(open, None);
    }

    #[test]
    fn invalid_notification_payload_marks_monitor_unhealthy() {
        let mut state = State {
            epoch: 0,
            last_change: None,
            layout_changed: false,
            destroyed: vec![],
            menu: None,
            callback_failed: false,
        };
        // No OS observer or UI operation: emulate the ABI's invalid payload
        // against a valid owned callback context.
        unsafe {
            callback(
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null(),
                &mut state as *mut State as *mut c_void,
            )
        };
        assert!(state.callback_failed);
        assert_eq!(state.epoch, 0);
        assert!(state.menu.is_none());
    }
}
