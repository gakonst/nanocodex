//! Platform-neutral UIA request ownership and identity contracts. No COM pointer
//! crosses this boundary; the actual provider is constructed and dropped by its
//! worker, and every mutation must check the request's cancellation deadline.
use crate::{Error, Result, ax::Node, selection::TextRange};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Root {
    pub hwnd: usize,
    pub pid: u32,
}
impl Root {
    pub fn validate(self) -> Result<()> {
        if self.hwnd == 0 || self.pid == 0 {
            return Err(Error::invalid("UIA requires a concrete HWND and PID"));
        }
        Ok(())
    }
}
/// A UIA clickable point belongs to one observed desktop frame. Keep its
/// absolute coordinates through activation instead of translating a stale
/// relative point using a newer window frame.
#[derive(Clone, Copy, Debug)]
pub struct BoundPoint {
    root: Root,
    frame: [f64; 4],
    screen: [f64; 2],
}
impl BoundPoint {
    pub fn new(root: Root, frame: [f64; 4], screen: [f64; 2]) -> Result<Self> {
        root.validate()?;
        crate::native::window_point(frame, [screen[0] - frame[0], screen[1] - frame[1]])?;
        Ok(Self {
            root,
            frame,
            screen,
        })
    }
    pub fn validate(self, current: Root, frame: [f64; 4], foreground: usize) -> Result<[f64; 2]> {
        if current != self.root || frame != self.frame || foreground != self.root.hwnd {
            return Err(Error::action(
                "UIA point window identity, geometry or activation changed before input",
            ));
        }
        Ok(self.screen)
    }
}
#[derive(Clone, Debug)]
pub enum Operation {
    Point,
    Invoke,
    SetValue(String),
    Toggle,
    Expand,
    Collapse,
    Select,
    AddSelection,
    RemoveSelection,
    ScrollIntoView,
    Focus,
    Scroll { horizontal: i32, vertical: i32 },
    SelectText(TextRange),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeIdentity {
    pub root: Root,
    pub root_runtime: Vec<i32>,
    pub process: i32,
    pub runtime: Vec<i32>,
}
impl RuntimeIdentity {
    pub fn validate(&self) -> Result<()> {
        self.root.validate()?;
        if self.process <= 0
            || self.runtime.is_empty()
            || self.root_runtime.is_empty()
            || self.runtime.len() > 128
            || self.root_runtime.len() > 128
        {
            return Err(Error::action("UIA runtime identity is missing or invalid"));
        }
        Ok(())
    }
    pub fn key(&self) -> String {
        format!(
            "uia:{}:{}:{}:{}:{}",
            self.root.hwnd,
            self.root.pid,
            self.root_runtime
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join("."),
            self.process,
            self.runtime
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(".")
        )
    }
}
pub fn revalidate(
    saved: &RuntimeIdentity,
    current: &RuntimeIdentity,
    before: &Node,
    after: &Node,
    operation: &Operation,
) -> Result<()> {
    saved.validate()?;
    current.validate()?;
    if saved != current {
        return Err(Error::action(
            "UIA element or window runtime identity changed",
        ));
    }
    if !before.semantic_eq(after, matches!(operation, Operation::SetValue(_))) {
        return Err(Error::action(
            "UIA element semantics changed; request a fresh window state",
        ));
    }
    if !after.enabled {
        return Err(Error::action("UIA target is disabled"));
    }
    Ok(())
}
pub fn range_value(value: &str, minimum: f64, maximum: f64, read_only: bool) -> Result<f64> {
    if read_only {
        return Err(Error::action("UIA range value is read-only"));
    }
    let value = value
        .parse::<f64>()
        .map_err(|_| Error::invalid("UIA range value must be numeric"))?;
    if ![value, minimum, maximum].iter().all(|v| v.is_finite())
        || minimum > maximum
        || value < minimum
        || value > maximum
    {
        return Err(Error::invalid(
            "UIA range value is outside its finite bounds",
        ));
    }
    Ok(value)
}
pub fn secondary(name: &str) -> Result<Operation> {
    match name {
        "AXPress" | "Invoke" | "invoke" => Ok(Operation::Invoke),
        "Toggle" | "toggle" => Ok(Operation::Toggle),
        "Expand" | "expand" => Ok(Operation::Expand),
        "Collapse" | "collapse" => Ok(Operation::Collapse),
        "Select" | "select" => Ok(Operation::Select),
        "AddToSelection" => Ok(Operation::AddSelection),
        "RemoveFromSelection" => Ok(Operation::RemoveSelection),
        "ScrollIntoView" | "AXScrollToVisible" => Ok(Operation::ScrollIntoView),
        "SetFocus" => Ok(Operation::Focus),
        "ScrollUp" | "AXScrollUpByPage" => Ok(Operation::Scroll {
            horizontal: 0,
            vertical: -2,
        }),
        "ScrollDown" | "AXScrollDownByPage" => Ok(Operation::Scroll {
            horizontal: 0,
            vertical: 2,
        }),
        "ScrollLeft" | "AXScrollLeftByPage" => Ok(Operation::Scroll {
            horizontal: -2,
            vertical: 0,
        }),
        "ScrollRight" | "AXScrollRightByPage" => Ok(Operation::Scroll {
            horizontal: 2,
            vertical: 0,
        }),
        _ => Err(Error::unsupported(format!(
            "Unsupported UIA action: {name}"
        ))),
    }
}

pub struct RequestContext {
    deadline: Instant,
    closed: Arc<AtomicBool>,
}
impl RequestContext {
    pub fn check(&self) -> Result<()> {
        if self.closed.load(Ordering::Acquire) || Instant::now() >= self.deadline {
            return Err(Error::new(
                -32008,
                "UIA request expired or worker was closed",
            ));
        }
        Ok(())
    }
}
/// Deliberately has no Send bound: COM interfaces remain entirely in their MTA.
pub trait Provider {
    fn observe(&mut self, root: Root, ctx: &RequestContext) -> Result<Node>;
    fn perform(
        &mut self,
        root: Root,
        identity: &str,
        operation: Operation,
        ctx: &RequestContext,
    ) -> Result<Option<[f64; 2]>>;
    fn clear(&mut self) -> Result<()>;
}
enum Request {
    Observe(Root),
    Perform(Root, String, Operation),
    Clear,
}
enum Reply {
    Tree(Box<Node>),
    Point(Option<[f64; 2]>),
    Cleared,
}
struct Envelope {
    request: Request,
    deadline: Instant,
    reply: SyncSender<Result<Reply>>,
}
pub struct Worker {
    requests: Option<SyncSender<Envelope>>,
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
            return Err(Error::invalid("Invalid UIA worker timeout"));
        }
        let (tx, rx) = mpsc::sync_channel::<Envelope>(8);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let flag = closed.clone();
        let thread = thread::Builder::new()
            .name("skyre-uia-mta".into())
            .spawn(move || {
                let mut provider = match factory() {
                    Ok(provider) => {
                        let _ = ready_tx.send(Ok(()));
                        provider
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                };
                while let Ok(envelope) = rx.recv() {
                    let ctx = RequestContext {
                        deadline: envelope.deadline,
                        closed: flag.clone(),
                    };
                    let result = ctx
                        .check()
                        .and_then(|()| match envelope.request {
                            Request::Observe(root) => provider
                                .observe(root, &ctx)
                                .map(|node| Reply::Tree(Box::new(node))),
                            Request::Perform(root, identity, op) => provider
                                .perform(root, &identity, op, &ctx)
                                .map(Reply::Point),
                            Request::Clear => provider.clear().map(|()| Reply::Cleared),
                        })
                        .and_then(|reply| ctx.check().map(|()| reply));
                    let _ = envelope.reply.send(result);
                    if flag.load(Ordering::Acquire) {
                        break;
                    }
                }
                // Release retained interfaces before the provider's COM apartment guard.
                let _ = provider.clear();
            })?;
        let mut worker = Self {
            requests: Some(tx),
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
                    "UIA MTA initialization timed out or disconnected",
                ))
            }
        }
    }
    fn call(&mut self, request: Request) -> Result<Reply> {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::new(
                -32008,
                "UIA worker is unavailable after timeout or shutdown",
            ));
        }
        let (reply, rx) = mpsc::sync_channel(1);
        self.requests
            .as_ref()
            .ok_or_else(|| Error::action("UIA worker is closed"))?
            .try_send(Envelope {
                request,
                deadline: Instant::now() + self.timeout,
                reply,
            })
            .map_err(|_| Error::action("UIA worker queue is full or disconnected"))?;
        match rx.recv_timeout(self.timeout) {
            Ok(result) => {
                if result.as_ref().is_err_and(|error| error.code == -32008) {
                    self.close();
                }
                result
            }
            Err(_) => {
                self.close();
                Err(Error::new(
                    -32008,
                    "UIA provider timed out; worker disabled, action completion may be unknown",
                ))
            }
        }
    }
    pub fn observe(&mut self, root: Root) -> Result<Node> {
        root.validate()?;
        match self.call(Request::Observe(root))? {
            Reply::Tree(node) => Ok(*node),
            _ => Err(Error::action("Invalid UIA observation reply")),
        }
    }
    pub fn perform(
        &mut self,
        root: Root,
        identity: &str,
        operation: Operation,
    ) -> Result<Option<[f64; 2]>> {
        root.validate()?;
        match self.call(Request::Perform(root, identity.into(), operation))? {
            Reply::Point(point) => Ok(point),
            _ => Err(Error::action("Invalid UIA action reply")),
        }
    }
    pub fn clear(&mut self) -> Result<()> {
        match self.call(Request::Clear)? {
            Reply::Cleared => Ok(()),
            _ => Err(Error::action("Invalid UIA clear reply")),
        }
    }
    fn close(&mut self) {
        self.closed.store(true, Ordering::Release);
        self.requests.take();
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
        // Never block host shutdown on a hung external COM provider. Its worker
        // remains disabled; hard termination requires the separate process tier.
    }
}

/// Locate a provider-defined character endpoint by its observed UTF-16 prefix.
/// The probe returns the actual unit count, prefix and an owned endpoint object.
/// A request inside a multi-code-unit character is rejected without selecting.
pub fn locate_text_endpoint<T>(
    source: &[u16],
    offset: usize,
    mut probe: impl FnMut(usize) -> Result<(usize, Vec<u16>, T)>,
) -> Result<T> {
    if offset > source.len() {
        return Err(Error::invalid("UIA endpoint is out of bounds"));
    }
    String::from_utf16(&source[..offset])
        .map_err(|_| Error::invalid("UIA endpoint splits a surrogate"))?;
    let (mut low, mut high) = (0usize, source.len());
    for _ in 0..32 {
        if low > high {
            break;
        }
        let count = low + (high - low) / 2;
        let (moved, prefix, endpoint) = probe(count)?;
        if moved > count {
            return Err(Error::action("UIA text provider returned invalid movement"));
        }
        if prefix.len() > source.len() || source[..prefix.len()] != prefix {
            return Err(Error::action(
                "UIA text changed while locating the endpoint",
            ));
        }
        if prefix.len() == offset {
            return Ok(endpoint);
        }
        if prefix.len() < offset {
            if moved != count {
                break;
            }
            low = count + 1;
        } else {
            if count == 0 {
                break;
            }
            high = count - 1;
        }
    }
    Err(Error::unsupported(
        "Requested UTF-16 offset is not an addressable UIA text-unit boundary",
    ))
}
