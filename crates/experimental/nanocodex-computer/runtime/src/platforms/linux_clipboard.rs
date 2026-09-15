//! X11 selection transactions served on a dedicated connection. Formats are
//! backed up before ownership changes; another owner's replacement always wins.
use crate::{Error, Result};
use std::{
    collections::BTreeMap,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{Event, res::ConnectionExt as _, xproto::*},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
};
use zeroize::Zeroize;

const LIMIT: usize = 8 * 1024 * 1024;
const CHUNK: usize = 32 * 1024;
type Response = mpsc::SyncSender<Result<()>>;
enum Command {
    Begin(String, Window, Response),
    Finish(bool, Response),
    Shutdown,
}
#[derive(Clone)]
struct Data {
    kind: Atom,
    format: u8,
    bytes: Vec<u8>,
}
impl Drop for Data {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}
struct Transfer {
    data: Data,
    offset: usize,
    deadline: Instant,
    paste: bool,
}
struct Active {
    backup: BTreeMap<Atom, Data>,
    previous_none: bool,
    consumed: bool,
    deadline: Instant,
    recipient_base: u32,
    recipient_mask: u32,
}

pub struct Clipboard {
    sender: mpsc::Sender<Command>,
}
impl Clipboard {
    pub fn new() -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let (ready, result) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("skyre-x11-selection".into())
            .spawn(move || match Owner::new() {
                Ok(mut owner) => {
                    let _ = ready.send(Ok(()));
                    owner.run(receiver);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })?;
        result
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| Error::action("X11 selection worker did not initialize"))??;
        Ok(Self { sender })
    }
    pub fn begin(&self, text: &str, recipient: Window) -> Result<()> {
        let (send, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Begin(text.into(), recipient, send))
            .map_err(|_| Error::action("X11 selection worker stopped"))?;
        receive
            .recv_timeout(Duration::from_secs(12))
            .map_err(|_| Error::action("X11 clipboard snapshot timed out"))?
    }
    pub fn finish(&self, wait: bool) -> Result<()> {
        let (send, receive) = mpsc::sync_channel(1);
        self.sender
            .send(Command::Finish(wait, send))
            .map_err(|_| Error::action("X11 selection worker stopped"))?;
        receive
            .recv_timeout(Duration::from_secs(6))
            .map_err(|_| Error::action("X11 clipboard restoration timed out"))?
    }
}
impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = self.sender.send(Command::Shutdown);
    }
}

fn fail(e: impl std::fmt::Display) -> Error {
    Error::action(format!("X11 clipboard: {e}"))
}
// Only the selection-owner check and replacement run while grabbed. No event
// waits, format transfers or sleeps are permitted in this critical section.
// Drop always queues an ungrab, including protocol-error and unwind paths.
struct ServerGrab<'a> {
    connection: &'a RustConnection,
    armed: bool,
}
impl<'a> ServerGrab<'a> {
    fn acquire(connection: &'a RustConnection) -> Result<Self> {
        let grab = Self {
            connection,
            armed: true,
        };
        connection
            .grab_server()
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        Ok(grab)
    }
    fn release(mut self) -> Result<()> {
        self.connection
            .ungrab_server()
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        self.armed = false;
        Ok(())
    }
}
impl Drop for ServerGrab<'_> {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(cookie) = self.connection.ungrab_server() {
                cookie.ignore_error();
            }
            let _ = self.connection.flush();
        }
    }
}
struct Owner {
    connection: RustConnection,
    window: Window,
    selection: Atom,
    targets: Atom,
    multiple: Atom,
    atom_pair: Atom,
    timestamp: Atom,
    incr: Atom,
    property: Atom,
    utf8: Atom,
    text: Atom,
    acquired: Timestamp,
    store: BTreeMap<Atom, Data>,
    active: Option<Active>,
    transfers: BTreeMap<(Window, Atom), Transfer>,
    delivered: BTreeMap<(Window, Atom), (Instant, bool)>,
}
impl Owner {
    fn new() -> Result<Self> {
        let (connection, screen) = x11rb::connect(None).map_err(fail)?;
        let window = connection.generate_id().map_err(fail)?;
        connection
            .create_window(
                0,
                window,
                connection.setup().roots[screen].root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                0,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        let atom = |name: &[u8]| -> Result<Atom> {
            Ok(connection
                .intern_atom(false, name)
                .map_err(fail)?
                .reply()
                .map_err(fail)?
                .atom)
        };
        Ok(Self {
            window,
            selection: atom(b"CLIPBOARD")?,
            targets: atom(b"TARGETS")?,
            multiple: atom(b"MULTIPLE")?,
            atom_pair: atom(b"ATOM_PAIR")?,
            timestamp: atom(b"TIMESTAMP")?,
            incr: atom(b"INCR")?,
            property: atom(b"_SKYRE_SELECTION_DATA")?,
            utf8: atom(b"UTF8_STRING")?,
            text: atom(b"TEXT")?,
            acquired: 0,
            store: BTreeMap::new(),
            active: None,
            transfers: BTreeMap::new(),
            delivered: BTreeMap::new(),
            connection,
        })
    }
    fn owner(&self) -> Result<Window> {
        Ok(self
            .connection
            .get_selection_owner(self.selection)
            .map_err(fail)?
            .reply()
            .map_err(fail)?
            .owner)
    }
    fn replace_owner(
        &self,
        expected: Window,
        replacement: Window,
        time: Timestamp,
    ) -> Result<bool> {
        let grab = ServerGrab::acquire(&self.connection)?;
        let result = (|| -> Result<bool> {
            if self.owner()? != expected {
                return Ok(false);
            }
            self.connection
                .set_selection_owner(replacement, self.selection, time)
                .map_err(fail)?
                .check()
                .map_err(fail)?;
            Ok(self.owner()? == replacement)
        })();
        let release = grab.release();
        let changed = result?;
        release?;
        Ok(changed)
    }
    fn now(&mut self, deadline: Instant) -> Result<Timestamp> {
        self.connection
            .change_property8(
                PropMode::REPLACE,
                self.window,
                self.property,
                AtomEnum::STRING,
                b"time",
            )
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        loop {
            if Instant::now() >= deadline {
                return Err(fail("server timestamp timed out"));
            }
            if let Some(event) = self.connection.poll_for_event().map_err(fail)? {
                if let Event::PropertyNotify(e) = event
                    && e.window == self.window
                    && e.atom == self.property
                    && e.state == Property::NEW_VALUE
                {
                    return Ok(e.time);
                }
                self.event(event)?;
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    fn read(&self, window: Window, property: Atom, delete: bool) -> Result<Data> {
        let reply = self
            .connection
            .get_property(
                delete,
                window,
                property,
                AtomEnum::ANY,
                0,
                (LIMIT / 4) as u32,
            )
            .map_err(fail)?
            .reply()
            .map_err(fail)?;
        if reply.bytes_after != 0 || reply.value.len() > LIMIT {
            return Err(fail("selection exceeds 8 MiB"));
        }
        if ![8, 16, 32].contains(&reply.format) {
            return Err(fail("selection has no transferable data"));
        }
        Ok(Data {
            kind: reply.type_,
            format: reply.format,
            bytes: reply.value,
        })
    }
    fn request(&mut self, target: Atom, deadline: Instant) -> Result<Data> {
        self.connection
            .delete_property(self.window, self.property)
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        self.connection
            .convert_selection(
                self.window,
                self.selection,
                target,
                self.property,
                x11rb::CURRENT_TIME,
            )
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        let mut incremental = false;
        let mut output: Option<Data> = None;
        loop {
            if Instant::now() >= deadline {
                return Err(fail("selection owner did not finish transfer"));
            }
            if let Some(event) = self.connection.poll_for_event().map_err(fail)? {
                let read = match &event {
                    Event::SelectionNotify(e)
                        if e.requestor == self.window
                            && e.selection == self.selection
                            && e.target == target =>
                    {
                        if e.property == 0 {
                            return Err(fail("selection owner refused advertised format"));
                        }
                        true
                    }
                    Event::PropertyNotify(e)
                        if incremental
                            && e.window == self.window
                            && e.atom == self.property
                            && e.state == Property::NEW_VALUE =>
                    {
                        true
                    }
                    _ => false,
                };
                if read {
                    let data = self.read(self.window, self.property, true)?;
                    if !incremental && data.kind == self.incr {
                        if data.format != 32 || data.bytes.len() != 4 {
                            return Err(fail("invalid INCR header"));
                        }
                        if u32::from_ne_bytes(data.bytes[..4].try_into().unwrap()) as usize > LIMIT
                        {
                            return Err(fail("INCR selection exceeds 8 MiB"));
                        }
                        incremental = true;
                    } else if incremental {
                        let empty = data.bytes.is_empty();
                        let result = output.get_or_insert_with(|| Data {
                            kind: data.kind,
                            format: data.format,
                            bytes: Vec::new(),
                        });
                        if result.kind != data.kind
                            || result.format != data.format
                            || result.bytes.len() + data.bytes.len() > LIMIT
                        {
                            return Err(fail("invalid or oversized INCR chunks"));
                        }
                        result.bytes.extend_from_slice(&data.bytes);
                        if empty {
                            return Ok(output.unwrap());
                        }
                    } else {
                        return Ok(data);
                    }
                } else {
                    self.event(event)?;
                }
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    fn begin(&mut self, text: String, recipient: Window) -> Result<()> {
        if self.active.is_some() {
            return Err(fail("another paste transaction is active"));
        }
        if text.len() > LIMIT {
            return Err(Error::invalid("text exceeds 8 MiB"));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        self.connection
            .res_query_version(1, 0)
            .map_err(fail)?
            .reply()
            .map_err(fail)?;
        let clients = self
            .connection
            .res_query_clients()
            .map_err(fail)?
            .reply()
            .map_err(fail)?
            .clients;
        let recipient = clients
            .into_iter()
            .find(|client| {
                recipient & !client.resource_mask == client.resource_base
                    && client.resource_base != 0
            })
            .ok_or_else(|| fail("focused window has no identifiable X11 client"))?;
        // Bound acquisition to the start of the snapshot. A newer ownership
        // epoch must win even if the owner reuses the same window ID.
        let time = self.now(deadline)?;
        let previous = self.owner()?;
        let backup = if previous == self.window {
            self.store.clone()
        } else if previous == 0 {
            BTreeMap::new()
        } else {
            let targets = self.request(self.targets, deadline)?;
            if targets.format != 32
                || targets.kind != u32::from(AtomEnum::ATOM)
                || targets.bytes.len() > 128 * 4
            {
                return Err(fail("invalid or oversized TARGETS list"));
            }
            let versioned = targets
                .bytes
                .chunks_exact(4)
                .any(|value| u32::from_ne_bytes(value.try_into().unwrap()) == self.timestamp);
            let timestamp = if versioned {
                Some(self.request(self.timestamp, deadline)?)
            } else {
                None
            };
            let mut backup = BTreeMap::new();
            let mut size = 0;
            for chunk in targets.bytes.chunks_exact(4) {
                let target = u32::from_ne_bytes(chunk.try_into().unwrap());
                let name = self
                    .connection
                    .get_atom_name(target)
                    .map_err(fail)?
                    .reply()
                    .map_err(fail)?
                    .name;
                // Metadata is generated by our owner; side-effect/resource targets are
                // not materializable clipboard values. Reject before taking ownership.
                if [self.targets, self.multiple, self.timestamp].contains(&target) {
                    continue;
                }
                if [
                    b"SAVE_TARGETS".as_slice(),
                    b"DELETE",
                    b"INSERT_SELECTION",
                    b"INSERT_PROPERTY",
                ]
                .contains(&name.as_slice())
                {
                    continue;
                }
                let data = self.request(target, deadline)?;
                let kind = self
                    .connection
                    .get_atom_name(data.kind)
                    .map_err(fail)?
                    .reply()
                    .map_err(fail)?
                    .name;
                if [
                    b"PIXMAP".as_slice(),
                    b"BITMAP",
                    b"DRAWABLE",
                    b"COLORMAP",
                    b"WINDOW",
                ]
                .contains(&kind.as_slice())
                {
                    return Err(fail(
                        "clipboard contains server resources that cannot be safely restored",
                    ));
                }
                size += data.bytes.len();
                if size > LIMIT {
                    return Err(fail("clipboard formats exceed 8 MiB"));
                }
                backup.insert(target, data);
            }
            if self.owner()? != previous {
                return Err(fail("clipboard owner changed while copying formats"));
            }
            if let Some(before) = timestamp {
                let after = self.request(self.timestamp, deadline)?;
                if before.kind != after.kind
                    || before.format != after.format
                    || before.bytes != after.bytes
                {
                    return Err(fail("clipboard contents changed while copying formats"));
                }
            }
            backup
        };
        if !self.replace_owner(previous, self.window, time)? {
            return Err(fail("could not acquire clipboard ownership"));
        }
        self.acquired = time;
        self.store.clear();
        let bytes = text.into_bytes();
        self.store.insert(
            self.utf8,
            Data {
                kind: self.utf8,
                format: 8,
                bytes: bytes.clone(),
            },
        );
        self.store.insert(
            self.text,
            Data {
                kind: self.utf8,
                format: 8,
                bytes: bytes.clone(),
            },
        );
        if bytes.is_ascii() {
            self.store.insert(
                AtomEnum::STRING.into(),
                Data {
                    kind: AtomEnum::STRING.into(),
                    format: 8,
                    bytes,
                },
            );
        }
        for value in self.delivered.values_mut() {
            value.1 = false;
        }
        for value in self.transfers.values_mut() {
            value.paste = false;
        }
        self.active = Some(Active {
            backup,
            previous_none: previous == 0,
            consumed: false,
            recipient_base: recipient.resource_base,
            recipient_mask: recipient.resource_mask,
            deadline: Instant::now() + Duration::from_secs(8),
        });
        Ok(())
    }
    fn finish(&mut self, wait: bool) -> Result<()> {
        if self.active.is_none() {
            return Err(fail("paste transaction is not active"));
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut failure = None;
        if wait {
            while !self.active.as_ref().unwrap().consumed
                && self.owner()? == self.window
                && Instant::now() < deadline
            {
                if let Some(event) = self.connection.poll_for_event().map_err(fail)? {
                    if let Err(error) = self.event(event) {
                        failure = Some(error);
                        break;
                    }
                } else {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            if !self.active.as_ref().unwrap().consumed {
                failure.get_or_insert_with(|| {
                    fail("paste recipient did not acknowledge clipboard data")
                });
            }
        }
        let still_owner = self.owner()? == self.window;
        let active = self.active.take().unwrap();
        if still_owner {
            self.store = active.backup;
            if active.previous_none {
                self.replace_owner(self.window, 0, self.acquired)?;
            }
            // Retain existing ownership while restoring its formats. Reacquiring
            // with a fresh timestamp could steal a competing client's selection.
        }
        failure.map_or(Ok(()), Err)
    }
    fn write(&self, window: Window, property: Atom, data: &Data) -> Result<()> {
        self.connection
            .change_property(
                PropMode::REPLACE,
                window,
                property,
                data.kind,
                data.format,
                (data.bytes.len() / (data.format as usize / 8)) as u32,
                &data.bytes,
            )
            .map_err(fail)?
            .check()
            .map_err(fail)
    }
    fn offer(&mut self, window: Window, property: Atom, target: Atom) -> Result<bool> {
        if property == 0 || self.transfers.len() + self.delivered.len() >= 128 {
            return Ok(false);
        }
        let data = if target == self.targets {
            let atoms: Vec<_> = self
                .store
                .keys()
                .copied()
                .chain([self.targets, self.timestamp, self.multiple])
                .collect();
            Data {
                kind: AtomEnum::ATOM.into(),
                format: 32,
                bytes: atoms.iter().flat_map(|n| n.to_ne_bytes()).collect(),
            }
        } else if target == self.timestamp {
            Data {
                kind: AtomEnum::INTEGER.into(),
                format: 32,
                bytes: self.acquired.to_ne_bytes().to_vec(),
            }
        } else if let Some(data) = self.store.get(&target) {
            data.clone()
        } else {
            return Ok(false);
        };
        let paste = self
            .active
            .as_ref()
            .is_some_and(|active| window & !active.recipient_mask == active.recipient_base)
            && [self.utf8, self.text, AtomEnum::STRING.into()].contains(&target);
        if self
            .transfers
            .values()
            .map(|v| v.data.bytes.len())
            .sum::<usize>()
            + data.bytes.len()
            > LIMIT * 2
        {
            return Ok(false);
        }
        self.connection
            .change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        if data.bytes.len() > CHUNK {
            self.connection
                .change_property32(
                    PropMode::REPLACE,
                    window,
                    property,
                    self.incr,
                    &[data.bytes.len() as u32],
                )
                .map_err(fail)?
                .check()
                .map_err(fail)?;
            self.transfers.insert(
                (window, property),
                Transfer {
                    data,
                    offset: 0,
                    deadline,
                    paste,
                },
            );
        } else {
            self.write(window, property, &data)?;
            self.delivered.insert((window, property), (deadline, paste));
        }
        Ok(true)
    }
    fn selection(&mut self, event: SelectionRequestEvent) -> Result<()> {
        let mut property = if event.property == 0 {
            event.target
        } else {
            event.property
        };
        if event.selection != self.selection
            || self.owner()? != self.window
            || (event.time != 0 && (event.time.wrapping_sub(self.acquired) as i32) < 0)
        {
            property = 0;
        } else if event.target == self.multiple {
            if let Ok(mut pairs) = self.read(event.requestor, property, false)
                && pairs.kind == self.atom_pair
                && pairs.format == 32
                && pairs.bytes.len() % 8 == 0
                && pairs.bytes.len() <= 128 * 8
            {
                for pair in pairs.bytes.chunks_exact_mut(8) {
                    let target = u32::from_ne_bytes(pair[..4].try_into().unwrap());
                    let destination = u32::from_ne_bytes(pair[4..].try_into().unwrap());
                    if target == self.multiple
                        || destination == property
                        || !self
                            .offer(event.requestor, destination, target)
                            .unwrap_or(false)
                    {
                        pair[4..].copy_from_slice(&0u32.to_ne_bytes());
                    }
                }
                self.write(event.requestor, property, &pairs)?;
            } else {
                property = 0;
            }
        } else if !self
            .offer(event.requestor, property, event.target)
            .unwrap_or(false)
        {
            property = 0;
        }
        self.connection
            .send_event(
                false,
                event.requestor,
                EventMask::NO_EVENT,
                SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: event.time,
                    requestor: event.requestor,
                    selection: event.selection,
                    target: event.target,
                    property,
                },
            )
            .map_err(fail)?
            .check()
            .map_err(fail)?;
        // Inline byte properties are now independently owned by the requestor.
        // Some toolkits retain the property after reading; waiting for deletion
        // would falsely time out an already delivered paste. INCR is acknowledged
        // only after its final data chunk has been copied.
        if property != 0
            && self
                .delivered
                .iter()
                .any(|((window, _), (_, paste))| *window == event.requestor && *paste)
            && let Some(active) = self.active.as_mut()
        {
            active.consumed = true;
        }
        Ok(())
    }
    fn event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::SelectionRequest(event) => self.selection(event)?,
            Event::SelectionClear(event) if event.selection == self.selection => {
                if self.owner()? != self.window {
                    self.store.clear();
                }
            }
            Event::PropertyNotify(event) if event.state == Property::DELETE => {
                let key = (event.window, event.atom);
                if let Some((_, true)) = self.delivered.remove(&key)
                    && let Some(active) = self.active.as_mut()
                {
                    active.consumed = true;
                }
                if let Some(mut transfer) = self.transfers.remove(&key) {
                    let end = (transfer.offset + CHUNK).min(transfer.data.bytes.len());
                    let chunk = Data {
                        kind: transfer.data.kind,
                        format: transfer.data.format,
                        bytes: transfer.data.bytes[transfer.offset..end].to_vec(),
                    };
                    self.write(event.window, event.atom, &chunk)?;
                    if transfer.offset == end {
                        if transfer.paste
                            && let Some(active) = self.active.as_mut()
                        {
                            active.consumed = true;
                        }
                        self.delivered
                            .insert(key, (transfer.deadline, transfer.paste));
                    } else {
                        transfer.offset = end;
                        self.transfers.insert(key, transfer);
                    }
                }
            }
            _ => (),
        }
        Ok(())
    }
    fn run(&mut self, receiver: mpsc::Receiver<Command>) {
        let mut detached = false;
        let mut owner_check = Instant::now();
        loop {
            if !detached {
                match receiver.try_recv() {
                    Ok(Command::Begin(text, recipient, response)) => {
                        let result = self.begin(text, recipient);
                        let _ = response.send(result);
                    }
                    Ok(Command::Finish(wait, response)) => {
                        let result = self.finish(wait);
                        let _ = response.send(result);
                    }
                    Ok(Command::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                        if self.active.is_some() {
                            let _ = self.finish(false);
                        }
                        // A restored clipboard must remain serviceable after a host reset.
                        // Retain this bounded owner until another app claims it or process exit.
                        detached = true;
                    }
                    Err(mpsc::TryRecvError::Empty) => (),
                }
            }
            if self
                .active
                .as_ref()
                .is_some_and(|a| Instant::now() >= a.deadline)
            {
                let _ = self.finish(false);
            }
            self.transfers.retain(|_, v| Instant::now() < v.deadline);
            self.delivered.retain(|_, v| Instant::now() < v.0);
            match self.connection.poll_for_event() {
                Ok(Some(event)) => {
                    let _ = self.event(event);
                }
                Ok(None) => thread::sleep(Duration::from_millis(2)),
                Err(_) => break,
            }
            if detached && Instant::now() >= owner_check {
                if self.owner().ok() != Some(self.window) {
                    break;
                }
                owner_check = Instant::now() + Duration::from_secs(1);
            }
        }
    }
}
