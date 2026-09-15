use super::Control;
use std::{
    ffi::c_void,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
pub struct Heap {
    pub handle: v8::IsolateHandle,
    pub control: Arc<Control>,
    pub calls: usize,
    pub ceiling: usize,
}
pub extern "C" fn near_limit(data: *mut c_void, current: usize, initial: usize) -> usize {
    let state = unsafe { &mut *data.cast::<Heap>() };
    state.calls += 1;
    state.control.reason.store(2, Ordering::Release);
    state.handle.terminate_execution();
    if state.ceiling == 0 {
        state.ceiling = current.max(initial).saturating_add(16 << 20);
    }
    state.ceiling
}
pub struct Buffers {
    pub used: AtomicUsize,
    limit: usize,
    control: Arc<Control>,
    pub handle: Mutex<Option<v8::IsolateHandle>>,
}
impl Buffers {
    pub fn new(limit: usize, control: Arc<Control>) -> Arc<Self> {
        Arc::new(Self {
            used: AtomicUsize::new(0),
            limit,
            control,
            handle: Mutex::new(None),
        })
    }
}
unsafe extern "C" fn allocate(state: &Buffers, size: usize) -> *mut c_void {
    if state
        .used
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
            used.checked_add(size).filter(|next| *next <= state.limit)
        })
        .is_err()
    {
        state.control.reason.store(3, Ordering::Release);
        if let Some(handle) = &*state.handle.lock().unwrap() {
            handle.terminate_execution();
        }
        return std::ptr::null_mut();
    }
    let memory = unsafe { libc::calloc(size.max(1), 1) };
    if memory.is_null() {
        state.used.fetch_sub(size, Ordering::AcqRel);
    }
    memory
}
unsafe extern "C" fn free(state: &Buffers, data: *mut c_void, size: usize) {
    unsafe { libc::free(data) };
    state.used.fetch_sub(size, Ordering::AcqRel);
}
unsafe extern "C" fn drop_state(state: *const Buffers) {
    drop(unsafe { Arc::from_raw(state) });
}
static VTABLE: v8::RustAllocatorVtable<Buffers> = v8::RustAllocatorVtable {
    allocate,
    allocate_uninitialized: allocate,
    free,
    drop: drop_state,
};
pub fn allocator(state: &Arc<Buffers>) -> v8::UniqueRef<v8::Allocator> {
    unsafe { v8::new_rust_allocator(Arc::into_raw(state.clone()), &VTABLE) }
}
