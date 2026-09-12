//! Direct asynchronous libpulse render-monitor capture, on one recording thread.
//! No daemon/process spawning, default-source fallback, or microphone acquisition.
use super::{
    linux_audio_model::{self as model, FragmentLease, Monitor},
    windows_audio_model::{Audio, Control, Encoding, Format, MAX_PACKET_BYTES, Packet, Provider},
};
use crate::{Error, Result};
use libpulse_sys::{self as pa, context::introspect::*, mainloop::standard::*};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    ffi::{CStr, CString, c_char, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr, thread,
    time::{Duration, Instant},
};

fn name(value: *const c_char) -> Result<String> {
    if value.is_null() {
        return Err(Error::action("PulseAudio returned a missing device name"));
    }
    // The C API guarantees a terminated string during the callback. Bound the
    // owned copy and reject invalid UTF-8 instead of changing target identity.
    let value = unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| Error::action("Invalid PulseAudio device name encoding"))?;
    if value.is_empty() || value.len() > 4096 {
        return Err(Error::action("PulseAudio device name exceeds bounds"));
    }
    Ok(value.to_owned())
}
#[derive(Debug)]
enum Reply {
    Server(String),
    Sink {
        index: u32,
        name: String,
        source: u32,
        source_name: String,
    },
    Source {
        index: u32,
        name: String,
        sink: u32,
        sink_name: String,
        rate: u32,
    },
    Ack,
}
#[derive(Default)]
struct Query {
    reply: RefCell<Option<Result<Reply>>>,
    complete: Cell<bool>,
    count: Cell<u32>,
    panicked: Cell<bool>,
}
fn callback(data: *mut c_void, eol: i32, read: impl FnOnce() -> Result<Reply>) {
    if data.is_null() {
        return;
    }
    let query = unsafe { &*data.cast::<Query>() };
    let result = catch_unwind(AssertUnwindSafe(|| {
        if eol != 0 {
            query.complete.set(true);
            if eol < 0 {
                *query.reply.borrow_mut() =
                    Some(Err(Error::action("PulseAudio device query failed")));
            }
        } else {
            query.count.set(query.count.get().saturating_add(1));
            *query.reply.borrow_mut() = Some(if query.count.get() == 1 {
                read()
            } else {
                Err(Error::action("PulseAudio device query is ambiguous"))
            });
        }
    }));
    if result.is_err() {
        query.panicked.set(true);
    }
}
extern "C" fn server_callback(
    _: *mut pa::pa_context,
    info: *const pa_server_info,
    data: *mut c_void,
) {
    callback(data, 0, || {
        let info = unsafe { info.as_ref() }
            .ok_or_else(|| Error::action("PulseAudio server query failed"))?;
        Ok(Reply::Server(name(info.default_sink_name)?))
    });
    callback(data, 1, || unreachable!());
}
extern "C" fn sink_callback(
    _: *mut pa::pa_context,
    info: *const pa_sink_info,
    eol: i32,
    data: *mut c_void,
) {
    callback(data, eol, || {
        let info = unsafe { info.as_ref() }
            .ok_or_else(|| Error::action("PulseAudio sink query failed"))?;
        Ok(Reply::Sink {
            index: info.index,
            name: name(info.name)?,
            source: info.monitor_source,
            source_name: name(info.monitor_source_name)?,
        })
    });
}
extern "C" fn source_callback(
    _: *mut pa::pa_context,
    info: *const pa_source_info,
    eol: i32,
    data: *mut c_void,
) {
    callback(data, eol, || {
        let info = unsafe { info.as_ref() }
            .ok_or_else(|| Error::action("PulseAudio source query failed"))?;
        Ok(Reply::Source {
            index: info.index,
            name: name(info.name)?,
            sink: info.monitor_of_sink,
            sink_name: name(info.monitor_of_sink_name)?,
            rate: info.sample_spec.rate,
        })
    });
}
extern "C" fn ack_callback(_: *mut pa::pa_stream, success: i32, data: *mut c_void) {
    callback(data, 0, || {
        if success != 0 {
            Ok(Reply::Ack)
        } else {
            Err(Error::action("PulseAudio stream control rejected"))
        }
    });
    callback(data, 1, || unreachable!());
}
struct Operation {
    raw: *mut pa::pa_operation,
    query: Box<Query>,
}
impl Operation {
    fn new(make: impl FnOnce(*mut c_void) -> *mut pa::pa_operation) -> Result<Self> {
        let mut query = Box::<Query>::default();
        let raw = make((&mut *query as *mut Query).cast());
        if raw.is_null() {
            return Err(Error::action("PulseAudio operation creation failed"));
        }
        Ok(Self { raw, query })
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        // Cancel guarantees the callback cannot run later. The Box stays alive
        // until after cancellation and unref; no callback allocation is leaked.
        unsafe {
            pa::pa_operation_cancel(self.raw);
            pa::pa_operation_unref(self.raw);
        }
    }
}
#[derive(Default)]
struct Events {
    overflow: Cell<bool>,
    moved: Cell<bool>,
}
extern "C" fn overflow_callback(_: *mut pa::pa_stream, data: *mut c_void) {
    if let Some(events) = unsafe { data.cast::<Events>().as_ref() } {
        events.overflow.set(true);
    }
}
extern "C" fn moved_callback(_: *mut pa::pa_stream, data: *mut c_void) {
    if let Some(events) = unsafe { data.cast::<Events>().as_ref() } {
        events.moved.set(true);
    }
}

struct Loopback {
    mainloop: *mut pa_mainloop,
    context: *mut pa::pa_context,
    stream: *mut pa::pa_stream,
    events: Box<Events>,
    monitor: Option<Monitor>,
    format: Format,
    position: u64,
    running: bool,
    closed: bool,
    remaining: VecDeque<Packet>,
}
impl Loopback {
    fn open(control: &Control) -> Result<Self> {
        control.check_start()?;
        let mut this = Self {
            mainloop: ptr::null_mut(),
            context: ptr::null_mut(),
            stream: ptr::null_mut(),
            events: Box::default(),
            monitor: None,
            format: Format {
                rate: 48_000,
                channels: 2,
                bits: 32,
                valid_bits: 32,
                block_align: 8,
                channel_mask: 3,
                encoding: Encoding::Float,
            },
            position: 0,
            running: false,
            closed: false,
            remaining: VecDeque::new(),
        };
        this.mainloop = unsafe { pa_mainloop_new() };
        if this.mainloop.is_null() {
            return Err(Error::action("PulseAudio mainloop allocation failed"));
        }
        this.context = unsafe {
            pa::pa_context_new(
                pa_mainloop_get_api(this.mainloop),
                c"Skyre computer loopback".as_ptr(),
            )
        };
        if this.context.is_null() {
            return Err(Error::action("PulseAudio context allocation failed"));
        }
        this.check(
            unsafe {
                pa::pa_context_connect(
                    this.context,
                    ptr::null(),
                    pa::PA_CONTEXT_NOAUTOSPAWN,
                    ptr::null(),
                )
            },
            "connect to running audio server",
        )?;
        loop {
            control.check_start()?;
            this.step()?;
            match unsafe { pa::pa_context_get_state(this.context) } {
                pa::PA_CONTEXT_READY => break,
                pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED => {
                    return Err(this.error("context connection"));
                }
                _ => thread::sleep(Duration::from_millis(2)),
            }
        }
        let op = Operation::new(|data| unsafe {
            pa_context_get_server_info(this.context, Some(server_callback), data)
        })?;
        let Reply::Server(sink_name) =
            this.query(op, Some(control), Instant::now() + Duration::from_secs(3))?
        else {
            return Err(Error::action("Invalid PulseAudio server response"));
        };
        let sink_c = CString::new(sink_name.as_str())
            .map_err(|_| Error::action("Invalid PulseAudio sink name"))?;
        let op = Operation::new(|data| unsafe {
            pa_context_get_sink_info_by_name(
                this.context,
                sink_c.as_ptr(),
                Some(sink_callback),
                data,
            )
        })?;
        let Reply::Sink {
            index: sink,
            name: actual_sink_name,
            source,
            source_name,
        } = this.query(op, Some(control), Instant::now() + Duration::from_secs(3))?
        else {
            return Err(Error::action("Invalid PulseAudio sink response"));
        };
        if actual_sink_name != sink_name {
            return Err(Error::action("PulseAudio default sink identity changed"));
        }
        let source_c = CString::new(source_name.as_str())
            .map_err(|_| Error::action("Invalid PulseAudio monitor name"))?;
        let op = Operation::new(|data| unsafe {
            pa_context_get_source_info_by_name(
                this.context,
                source_c.as_ptr(),
                Some(source_callback),
                data,
            )
        })?;
        let Reply::Source {
            index,
            name,
            sink: monitor_sink,
            sink_name: monitor_sink_name,
            rate,
        } = this.query(op, Some(control), Instant::now() + Duration::from_secs(3))?
        else {
            return Err(Error::action("Invalid PulseAudio monitor response"));
        };
        let monitor = Monitor {
            sink,
            sink_name,
            source,
            source_name,
            rate,
        };
        monitor.validate(index, &name, monitor_sink, &monitor_sink_name)?;
        this.format.rate = rate;
        this.format.validate()?;
        let spec = pa::pa_sample_spec {
            format: pa::PA_SAMPLE_FLOAT32LE,
            rate,
            channels: 2,
        };
        let mut map = pa::pa_channel_map::default();
        unsafe {
            pa::pa_channel_map_init_stereo(&mut map);
        }
        this.stream = unsafe {
            pa::pa_stream_new(
                this.context,
                c"Computer audio (render monitor only)".as_ptr(),
                &spec,
                &map,
            )
        };
        if this.stream.is_null() {
            return Err(this.error("stream allocation"));
        }
        let events = (&mut *this.events as *mut Events).cast();
        unsafe {
            pa::pa_stream_set_overflow_callback(this.stream, Some(overflow_callback), events);
            pa::pa_stream_set_moved_callback(this.stream, Some(moved_callback), events);
        }
        let attr = pa::pa_buffer_attr {
            maxlength: MAX_PACKET_BYTES as u32,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: rate / 100 * 8,
        };
        control.check_start()?;
        this.check(
            unsafe {
                pa::pa_stream_connect_record(
                    this.stream,
                    source_c.as_ptr(),
                    &attr,
                    pa::PA_STREAM_START_CORKED
                        | pa::PA_STREAM_DONT_MOVE
                        | pa::PA_STREAM_ADJUST_LATENCY,
                )
            },
            "connect verified render monitor",
        )?;
        this.monitor = Some(monitor);
        loop {
            control.check_start()?;
            this.step()?;
            match unsafe { pa::pa_stream_get_state(this.stream) } {
                pa::PA_STREAM_READY => break,
                pa::PA_STREAM_FAILED | pa::PA_STREAM_TERMINATED => {
                    return Err(this.error("stream connection"));
                }
                _ => thread::sleep(Duration::from_millis(2)),
            }
        }
        this.validate_stream()?;
        let actual = unsafe { pa::pa_stream_get_sample_spec(this.stream).as_ref() }
            .ok_or_else(|| this.error("sample specification"))?;
        let actual_map = unsafe { pa::pa_stream_get_channel_map(this.stream).as_ref() }
            .ok_or_else(|| this.error("channel map"))?;
        if actual.format != spec.format
            || actual.rate != spec.rate
            || actual.channels != 2
            || actual_map.channels != 2
            || actual_map.map[0] != pa::PA_CHANNEL_POSITION_FRONT_LEFT
            || actual_map.map[1] != pa::PA_CHANNEL_POSITION_FRONT_RIGHT
        {
            return Err(Error::action(
                "PulseAudio changed requested stereo float PCM format",
            ));
        }
        Ok(this)
    }
    fn error(&self, action: &str) -> Error {
        let code = if self.context.is_null() {
            -1
        } else {
            unsafe { pa::pa_context_errno(self.context) }
        };
        let message = unsafe { pa::pa_strerror(code) };
        let message = if message.is_null() {
            "unknown native error".into()
        } else {
            unsafe { CStr::from_ptr(message) }
                .to_string_lossy()
                .into_owned()
        };
        Error::action(format!("PulseAudio {action} failed ({code}): {message}"))
    }
    fn check(&self, value: i32, action: &str) -> Result<()> {
        if value < 0 {
            Err(self.error(action))
        } else {
            Ok(())
        }
    }
    fn step(&mut self) -> Result<()> {
        if self.mainloop.is_null() {
            return Err(Error::action("PulseAudio mainloop is closed"));
        }
        self.check(
            unsafe { pa_mainloop_iterate(self.mainloop, 0, ptr::null_mut()) },
            "mainloop iteration",
        )?;
        if !self.context.is_null()
            && matches!(
                unsafe { pa::pa_context_get_state(self.context) },
                pa::PA_CONTEXT_FAILED | pa::PA_CONTEXT_TERMINATED
            )
        {
            return Err(self.error("server connection lost"));
        }
        Ok(())
    }
    fn query(
        &mut self,
        operation: Operation,
        control: Option<&Control>,
        deadline: Instant,
    ) -> Result<Reply> {
        loop {
            if let Some(control) = control {
                control.check_start()?;
            }
            if Instant::now() >= deadline {
                return Err(Error::new(-32008, "PulseAudio operation timed out"));
            }
            self.step()?;
            if operation.query.panicked.get() {
                return Err(Error::action("PulseAudio callback failed"));
            }
            match unsafe { pa::pa_operation_get_state(operation.raw) } {
                pa::PA_OPERATION_DONE if operation.query.complete.get() => {
                    return operation
                        .query
                        .reply
                        .borrow_mut()
                        .take()
                        .ok_or_else(|| Error::action("PulseAudio operation returned no result"))?;
                }
                pa::PA_OPERATION_DONE | pa::PA_OPERATION_CANCELLED => {
                    return Err(Error::action(
                        "PulseAudio operation incomplete or cancelled",
                    ));
                }
                _ => thread::sleep(Duration::from_millis(2)),
            }
        }
    }
    fn validate_stream(&self) -> Result<()> {
        if self.events.overflow.get() {
            return Err(Error::action(
                "PulseAudio capture overflow; recording is incomplete",
            ));
        }
        if self.events.moved.get() {
            return Err(Error::action(
                "PulseAudio recording was moved to another source",
            ));
        }
        if self.stream.is_null()
            || unsafe { pa::pa_stream_get_state(self.stream) } != pa::PA_STREAM_READY
        {
            return Err(self.error("stream is not ready"));
        }
        let monitor = self
            .monitor
            .as_ref()
            .ok_or_else(|| Error::action("PulseAudio monitor is unbound"))?;
        monitor.validate_stream(
            unsafe { pa::pa_stream_get_device_index(self.stream) },
            &name(unsafe { pa::pa_stream_get_device_name(self.stream) })?,
        )
    }
    fn copy_packet(&mut self) -> Result<Option<Packet>> {
        self.step()?;
        self.validate_stream()?;
        let available = unsafe { pa::pa_stream_readable_size(self.stream) };
        if available == usize::MAX {
            return Err(self.error("readable size"));
        }
        if available > MAX_PACKET_BYTES {
            return Err(Error::action("PulseAudio queued data exceeds bound"));
        }
        if available == 0 {
            return Ok(None);
        }
        let mut data: *const c_void = ptr::null();
        let mut bytes = 0;
        self.check(
            unsafe { pa::pa_stream_peek(self.stream, &mut data, &mut bytes) },
            "peek",
        )?;
        if bytes == 0 {
            return Ok(None);
        }
        let stream = self.stream;
        let lease = FragmentLease::new(|| {
            self.check(
                unsafe { pa::pa_stream_drop(stream) },
                "drop captured fragment",
            )
        });
        let result = (|| {
            if bytes > available || bytes > MAX_PACKET_BYTES {
                return Err(Error::action("PulseAudio fragment exceeds readable extent"));
            }
            let data = if data.is_null() {
                None
            } else {
                Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), bytes) })
            };
            model::fragment(data, bytes, self.format, self.position)
        })();
        let packet = lease.finish(result)?;
        if let Some(packet) = &packet {
            self.position += packet.frames as u64;
        }
        Ok(packet)
    }
    fn close(&mut self) -> Result<()> {
        let mut failure = None;
        if !self.stream.is_null() {
            unsafe {
                pa::pa_stream_set_overflow_callback(self.stream, None, ptr::null_mut());
                pa::pa_stream_set_moved_callback(self.stream, None, ptr::null_mut());
                if pa::pa_stream_disconnect(self.stream) < 0 {
                    failure = Some(self.error("disconnect stream"));
                }
                pa::pa_stream_unref(self.stream);
            }
            self.stream = ptr::null_mut();
        }
        if !self.context.is_null() {
            unsafe {
                pa::pa_context_disconnect(self.context);
                pa::pa_context_unref(self.context);
            }
            self.context = ptr::null_mut();
        }
        if !self.mainloop.is_null() {
            unsafe {
                pa_mainloop_free(self.mainloop);
            }
            self.mainloop = ptr::null_mut();
        }
        self.closed = true;
        self.running = false;
        failure.map_or(Ok(()), Err)
    }
}
impl Provider for Loopback {
    fn format(&self) -> Format {
        self.format
    }
    fn start(&mut self, control: &Control) -> Result<()> {
        control.check_start()?;
        self.validate_stream()?;
        let operation = Operation::new(|data| unsafe {
            pa::pa_stream_cork(self.stream, 0, Some(ack_callback), data)
        })?;
        self.query(
            operation,
            Some(control),
            Instant::now() + Duration::from_secs(3),
        )?;
        self.running = true;
        control.check_start()
    }
    fn next_packet(&mut self) -> Result<Option<Packet>> {
        if self.closed {
            Ok(self.remaining.pop_front())
        } else {
            self.copy_packet()
        }
    }
    fn wait(&mut self, timeout: Duration) -> Result<()> {
        self.step()?;
        self.validate_stream()?;
        thread::sleep(timeout.min(Duration::from_millis(5)));
        self.step()
    }
    fn stop(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        let result = (|| {
            if self.running {
                let operation = Operation::new(|data| unsafe {
                    pa::pa_stream_cork(self.stream, 1, Some(ack_callback), data)
                })?;
                self.query(operation, None, Instant::now() + Duration::from_secs(1))?;
                self.running = false;
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut bytes = 0usize;
            for count in 0..=128 {
                if count == 128 || Instant::now() >= deadline {
                    return Err(Error::new(
                        -32008,
                        "PulseAudio final packet drain exceeded bound",
                    ));
                }
                let Some(packet) = self.copy_packet()? else {
                    break;
                };
                bytes += packet.frames as usize * self.format.block_align as usize;
                if bytes > MAX_PACKET_BYTES {
                    return Err(Error::action("PulseAudio final queue exceeds bound"));
                }
                self.remaining.push_back(packet);
            }
            Ok(())
        })();
        let cleanup = self.close();
        match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) | (Ok(()), Err(error)) => {
                self.remaining.clear();
                Err(error)
            }
            (Err(mut error), Err(cleanup)) => {
                self.remaining.clear();
                error
                    .message
                    .push_str(&format!("; cleanup failed: {}", cleanup.message));
                Err(error)
            }
        }
    }
}
impl Drop for Loopback {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

pub fn recorder() -> Audio {
    Audio::new(|control| Ok(Box::new(Loopback::open(control)?)))
}
