//! Disposable loopback CDP fixture for cooperative raw-read integration tests.
use serde_json::{Value, json};
use std::{
    io::ErrorKind,
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
use tungstenite::Message;
pub struct Provider {
    pub endpoint: String,
    pub commands: Arc<Mutex<Vec<Value>>>,
    events: mpsc::Sender<Vec<Value>>,
    stop: Arc<AtomicBool>,
    socket: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Provider {
    pub fn start() -> Self {
        Self::with_attachment_event(None)
    }
    pub fn with_attachment_event(mut attachment_event: Option<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let commands = Arc::new(Mutex::new(vec![]));
        let recorded = commands.clone();
        let (events, incoming) = mpsc::channel::<Vec<Value>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let socket = Arc::new(Mutex::new(None));
        let socket_copy = socket.clone();
        let worker = thread::spawn(move || {
            let stream = loop {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            *socket_copy.lock().unwrap() = Some(stream.try_clone().unwrap());
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            ws.get_ref()
                .set_read_timeout(Some(Duration::from_millis(5)))
                .unwrap();
            let mut closed = false;
            while !stopped.load(Ordering::Relaxed) {
                for batch in incoming.try_iter() {
                    for event in batch {
                        if ws.send(Message::text(event.to_string())).is_err() {
                            return;
                        }
                    }
                }
                let message = match ws.read() {
                    Ok(message) => message,
                    Err(tungstenite::Error::Io(e))
                        if [ErrorKind::WouldBlock, ErrorKind::TimedOut].contains(&e.kind()) =>
                    {
                        continue;
                    }
                    Err(_) => return,
                };
                let Message::Text(text) = message else {
                    continue;
                };
                let request: Value = serde_json::from_str(&text).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let result = match request["method"].as_str().unwrap() {
                    "Target.createBrowserContext" => json!({"browserContextId":"owned-context"}),
                    "Target.getBrowserContexts" => json!({"browserContextIds":["owned-context"]}),
                    "Target.createTarget" => {
                        closed = false;
                        json!({"targetId":"t"})
                    }
                    "Target.attachToTarget" => {
                        json!({"sessionId":format!("root-{}",request["params"]["targetId"].as_str().unwrap())})
                    }
                    "Target.getTargets" => {
                        let targets = if closed {
                            vec![]
                        } else {
                            vec![
                                json!({"targetId":"t","type":"page","url":"https://owned.example/","title":"Owned","browserContextId":"owned-context"}),
                            ]
                        };
                        json!({"targetInfos":targets})
                    }
                    "Target.getTargetInfo" => {
                        json!({"targetInfo":{"targetId":"t","type":"page","url":"https://owned.example/","title":"Owned"}})
                    }
                    "Page.getFrameTree" => {
                        json!({"frameTree":{"frame":{"id":"frame-t","url":"https://owned.example/","loaderId":"owned-loader"}}})
                    }
                    "Page.createIsolatedWorld" => json!({"executionContextId":1}),
                    "Runtime.evaluate" => {
                        json!({"result":{"value":{"url":"https://owned.example/","timeOrigin":1}}})
                    }
                    "Target.closeTarget" => {
                        closed = true;
                        json!({"success":true})
                    }
                    "Page.enable" => {
                        if let Some(event) = attachment_event.take()
                            && ws.send(Message::text(event.to_string())).is_err()
                        {
                            return;
                        }
                        json!({})
                    }
                    "Fixture.emit" => {
                        for event in request["params"]["events"].as_array().unwrap() {
                            if ws.send(Message::text(event.to_string())).is_err() {
                                return;
                            }
                        }
                        json!({})
                    }
                    _ => json!({}),
                };
                if ws
                    .send(Message::text(
                        json!({"id":request["id"],"result":result}).to_string(),
                    ))
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            endpoint,
            commands,
            events,
            stop,
            socket,
            thread: Some(worker),
        }
    }
    pub fn emit(&self, events: Vec<Value>) {
        self.events.send(events).unwrap();
    }
    pub fn disconnect(&self) {
        if let Some(socket) = self.socket.lock().unwrap().as_ref() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
    pub fn wait_command(&self, method: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|r| r["method"] == method)
            {
                return;
            }
            assert!(Instant::now() < deadline, "Missing owned command {method}");
            thread::sleep(Duration::from_millis(2));
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.disconnect();
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
pub fn event(method: &str, n: u64) -> Value {
    json!({"sessionId":"root-t","method":method,"params":{"n":n}})
}
