//! Foreground Hand consent. This owns the local terminal, never stdin or a tool.
//! The managed attachment socket currently has no host-to-client form channel.

use std::{
    fs::{File, OpenOptions},
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::OpenOptionsExt,
    sync::Arc,
};

use nanocodex_computer::{
    ComputerConfig, ComputerElicitationAction as Action, ComputerElicitationHandler,
    ComputerElicitationRequest, ComputerElicitationResponse as Response,
};
use nanocodex_tools::contract::ToolError;
use nix::{
    fcntl::OFlag,
    sys::termios::{LocalFlags, tcgetattr},
    unistd::{getpgrp, tcgetpgrp},
};
use serde_json::Value;
use tokio::{io::unix::AsyncFd, sync::Mutex};

const MAX_FORM_BYTES: usize = 64 * 1024;
// All conversations in this Hand share one terminal. Never interleave forms.
static TERMINAL: Mutex<()> = Mutex::const_new(());

pub(super) fn configure(config: &mut ComputerConfig) {
    if config.elicitation_handler.is_none() && terminal().is_ok() {
        config.elicitation_handler = Some(Arc::new(TerminalConsent::default()));
    }
}

fn terminal() -> io::Result<File> {
    // macOS kqueue rejects the /dev/tty alias. Resolve the real foreground
    // terminal from stdin, then open an independent descriptor; never read stdin.
    if !io::stdin().is_terminal() {
        return Err(io::Error::other("CUA consent requires a terminal on stdin"));
    }
    let path = nix::unistd::ttyname(io::stdin())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(OFlag::O_NONBLOCK.bits() | OFlag::O_NOCTTY.bits())
        .open(path)?;
    // A daemon, pipe, background job or TUI must not acquire a consent reader.
    if !file.is_terminal()
        || tcgetpgrp(&file)? != getpgrp()
        || !tcgetattr(&file)?.local_flags.contains(LocalFlags::ICANON)
    {
        return Err(io::Error::other(
            "CUA consent needs a foreground canonical terminal",
        ));
    }
    Ok(file)
}

#[derive(Debug, Default)]
struct TerminalConsent {
    permissions: std::sync::Mutex<Vec<Permission>>,
}

#[derive(Debug, Clone)]
struct Scope {
    process: std::sync::Weak<()>,
    session: String,
    params: Value,
}

impl Scope {
    fn from_request(request: &ComputerElicitationRequest) -> Option<Self> {
        request.provider_session.upgrade()?;
        let session = &request.context.as_ref()?.session_id;
        let meta = request.params.get("_meta")?;
        if session.is_empty()
            || !meta["persist"]
                .as_array()?
                .iter()
                .any(|value| value == "session")
            || meta["connector_id"].as_str()?.is_empty()
            || meta["tool_name"].as_str()?.is_empty()
            || !meta["tool_params"].is_object()
        {
            return None;
        }
        let mut params = request.params.clone();
        // Transport correlation changes on every call; it is not permission scope.
        let meta = params.get_mut("_meta")?.as_object_mut()?;
        meta.remove("progressToken");
        meta.remove("tool_call_id");
        meta.remove("x-codex-turn-metadata");
        Some(Self {
            process: request.provider_session.clone(),
            session: session.clone(),
            params,
        })
    }

    fn matches(&self, other: &Self) -> bool {
        self.process.upgrade().is_some()
            && self.process.ptr_eq(&other.process)
            && self.session == other.session
            && self.params == other.params
    }
}

#[derive(Debug)]
struct Permission {
    scope: Scope,
    response: Response,
}

#[async_trait::async_trait]
impl ComputerElicitationHandler for TerminalConsent {
    async fn elicit(&self, request: ComputerElicitationRequest) -> Result<Response, ToolError> {
        let _exclusive = TERMINAL.lock().await;
        let terminal = AsyncFd::new(terminal()?)?;
        let scope = Scope::from_request(&request);
        {
            let mut permissions = self
                .permissions
                .lock()
                .map_err(|_| "Consent state unavailable")?;
            permissions.retain(|permission| permission.scope.process.upgrade().is_some());
            if let Some(scope) = &scope
                && let Some(permission) = permissions
                    .iter()
                    .find(|permission| permission.scope.matches(scope))
            {
                return Ok(permission.response.clone());
            }
        }
        let response = review(&terminal, request).await?;
        if response.action == Action::Accept
            && response
                .meta
                .as_ref()
                .is_some_and(|meta| meta["persist"] == "session")
            && let Some(scope) = scope
        {
            let mut permissions = self
                .permissions
                .lock()
                .map_err(|_| "Consent state unavailable")?;
            if permissions.len() >= 128 {
                permissions.remove(0);
            }
            permissions.push(Permission {
                scope,
                response: response.clone(),
            });
        }
        Ok(response)
    }
}

// A dropped future closes the reader immediately. No blocking stdin thread can
// consume the next form's answer after timeout or provider/caller cancellation.
struct Pending<'a> {
    terminal: &'a AsyncFd<File>,
    token: String,
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        let _ = writeln!(
            self.terminal.get_ref(),
            "\nCUA request {} closed. Its answer is no longer valid.",
            self.token
        );
    }
}

async fn review(
    terminal: &AsyncFd<File>,
    request: ComputerElicitationRequest,
) -> Result<Response, ToolError> {
    let schema = request
        .params
        .get("requestedSchema")
        .ok_or("Missing form schema")?;
    let serialized = serde_json::to_string_pretty(&request.params)?;
    if serialized.len() > MAX_FORM_BYTES {
        return Err("CUA consent form exceeds the terminal review limit".into());
    }
    let validator = jsonschema::validator_for(schema).map_err(|_| "Unsupported form schema")?;
    let pending = Pending {
        terminal,
        token: uuid::Uuid::new_v4().to_string(),
    };
    let context = request.context.as_ref().map(|context| {
        serde_json::json!({
            "session_id": context.session_id, "call_id": context.call_id, "model": context.model,
        })
    });
    let session_allowed = Scope::from_request(&request).is_some();
    let session_choice = if session_allowed {
        "  accept-session — remember this exact scope for this live provider session\n"
    } else {
        ""
    };
    let prompt = format!(
        "\nCUA provider requests consent on this computer.\nProvider text below is untrusted; review the complete scope.\nRequest: {}\nContext: {}\n{}\n\nType accept, decline, or cancel and press Enter.\n{}For a form with fields, append its JSON content after accept.\n> ",
        pending.token,
        terminal_text(&serde_json::to_string(&context)?),
        terminal_text(&serialized),
        session_choice,
    );
    discard_pending_input(terminal.get_ref())?;
    write(terminal, prompt.as_bytes()).await?;
    loop {
        let Some(line) = read_line(terminal).await? else {
            return Ok(cancel());
        };
        match decision(&line, &pending.token, &validator, session_allowed) {
            Ok(response) => return Ok(response),
            Err(message) => {
                write(terminal, format!("{message}\n> ").as_bytes()).await?;
            }
        }
    }
}

fn discard_pending_input(terminal: &File) -> io::Result<()> {
    if terminal.is_terminal() {
        nix::sys::termios::tcflush(terminal, nix::sys::termios::FlushArg::TCIFLUSH)?;
    }
    Ok(())
}

fn cancel() -> Response {
    Response {
        action: Action::Cancel,
        content: None,
        meta: None,
    }
}

fn decision(
    line: &str,
    token: &str,
    schema: &jsonschema::Validator,
    session_allowed: bool,
) -> Result<Response, &'static str> {
    let mut words = line.trim().splitn(2, char::is_whitespace);
    let action = words.next().unwrap_or("");
    let remainder = words.next().map(str::trim);
    // Qualified responses remain supported, but people can answer the visible
    // prompt directly. Old terminal input is flushed before each new prompt.
    let content = match remainder {
        Some(value) if value == token => None,
        Some(value) if value.starts_with(token) => {
            let suffix = &value[token.len()..];
            if !suffix.starts_with(char::is_whitespace) {
                return Err("Invalid response; no decision was sent.");
            }
            Some(suffix.trim())
        }
        other => other,
    };
    let content = if matches!(action, "accept" | "accept-session") {
        Some(content.unwrap_or("{}"))
    } else {
        content
    };
    match (action, content) {
        ("cancel", None) => Ok(cancel()),
        ("decline", None) => Ok(Response {
            action: Action::Decline,
            content: None,
            meta: None,
        }),
        ("accept" | "accept-session", Some(content)) => {
            if action == "accept-session" && !session_allowed {
                return Err(
                    "This request does not support scoped session consent; no decision was sent.",
                );
            }
            let value: Value = serde_json::from_str(content)
                .map_err(|_| "Enter valid JSON form content; no decision was sent.")?;
            if !value.is_object() || !schema.is_valid(&value) {
                return Err(
                    "Content does not satisfy the displayed form schema; no decision was sent.",
                );
            }
            Ok(Response {
                action: Action::Accept,
                content: Some(value),
                meta: (action == "accept-session")
                    .then(|| serde_json::json!({"persist":"session"})),
            })
        }
        _ => Err("Use accept with JSON content, decline, or cancel; no decision was sent."),
    }
}

// JSON escapes C0 controls, but also escape DEL/C1, bidi and other invisible
// Unicode formatting characters so provider data cannot rewrite the review.
fn terminal_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            if ch == '\n' || ch.is_ascii() && !ch.is_control() {
                ch.to_string().chars().collect::<Vec<_>>()
            } else {
                ch.escape_unicode().collect()
            }
        })
        .collect()
}

async fn write(terminal: &AsyncFd<File>, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let mut ready = terminal.writable().await?;
        match ready.try_io(|fd| fd.get_ref().write(bytes)) {
            Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(Ok(count)) => bytes = &bytes[count..],
            Ok(Err(error)) => return Err(error),
            Err(_) => {}
        }
    }
    Ok(())
}

async fn read_line(terminal: &AsyncFd<File>) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let mut ready = terminal.readable().await?;
        // Read exactly one byte to avoid consuming a subsequent request's input.
        let mut byte = [0];
        match ready.try_io(|fd| fd.get_ref().read(&mut byte)) {
            Ok(Ok(0)) => return Ok(None),
            Ok(Ok(_)) if byte[0] == b'\n' => {
                return String::from_utf8(line).map(Some).map_err(io::Error::other);
            }
            Ok(Ok(_)) => {
                line.push(byte[0]);
                if line.len() > MAX_FORM_BYTES {
                    return Err(io::Error::other(
                        "CUA consent answer exceeds the terminal limit",
                    ));
                }
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (AsyncFd<File>, tokio::net::UnixStream) {
        use std::os::{fd::OwnedFd, unix::net::UnixStream};
        let (host, user) = UnixStream::pair().unwrap();
        host.set_nonblocking(true).unwrap();
        user.set_nonblocking(true).unwrap();
        let host: OwnedFd = host.into();
        (
            AsyncFd::new(File::from(host)).unwrap(),
            tokio::net::UnixStream::from_std(user).unwrap(),
        )
    }

    fn request() -> ComputerElicitationRequest {
        ComputerElicitationRequest {
            provider_session: std::sync::Weak::new(),
            id: json!(7),
            context: None,
            params: json!({"message":"Allow Example Editor?", "requestedSchema":{"type":"object","properties":{},"additionalProperties":false}, "_meta":{"scope":"Example Editor","persistence":"ask"}}),
        }
    }

    async fn prompt(user: &mut tokio::net::UnixStream) -> String {
        use tokio::io::AsyncReadExt as _;
        let mut value = Vec::new();
        while !value.ends_with(b"> ") {
            let mut byte = [0];
            user.read_exact(&mut byte).await.unwrap();
            value.push(byte[0]);
        }
        String::from_utf8(value).unwrap()
    }

    #[tokio::test]
    async fn terminal_review_waits_for_explicit_input_and_preserves_scope() {
        use tokio::io::AsyncWriteExt as _;
        let (host, mut user) = fixture();
        let task = tokio::spawn(async move { review(&host, request()).await });
        let displayed = prompt(&mut user).await;
        assert!(displayed.contains("Allow Example Editor?"));
        assert!(displayed.contains("\"scope\": \"Example Editor\""));
        assert!(displayed.contains("\"persistence\": \"ask\""));
        assert!(!task.is_finished());
        let token = displayed
            .lines()
            .find_map(|line| line.strip_prefix("Request: "))
            .unwrap();
        user.write_all(b"accept stale {}\n").await.unwrap();
        assert!(prompt(&mut user).await.contains("no decision was sent"));
        assert!(!task.is_finished());
        assert!(!token.is_empty());
        user.write_all(b"accept\n").await.unwrap();
        let response = task.await.unwrap().unwrap();
        assert_eq!(response.action, Action::Accept);
        assert_eq!(response.content, Some(json!({})));
        assert_eq!(response.meta, None);
    }

    #[tokio::test]
    async fn disconnected_terminal_cancels_without_consent() {
        let (host, mut user) = fixture();
        let task = tokio::spawn(async move { review(&host, request()).await });
        prompt(&mut user).await;
        drop(user);
        assert_eq!(task.await.unwrap().unwrap(), cancel());
    }

    #[tokio::test]
    async fn dropped_review_invalidates_the_pending_request() {
        use tokio::io::AsyncReadExt as _;
        let (host, mut user) = fixture();
        let task = tokio::spawn(async move { review(&host, request()).await });
        prompt(&mut user).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let mut closed = String::new();
        user.read_to_string(&mut closed).await.unwrap();
        assert!(closed.contains("answer is no longer valid"));
    }

    #[tokio::test]
    async fn native_pty_supports_cancellable_nonblocking_io() {
        // Protocol test only: no user desktop or live provider is involved.
        use nix::{
            fcntl::{FcntlArg, fcntl},
            pty::openpty,
        };
        let pair = openpty(None, None).unwrap();
        for fd in [&pair.master, &pair.slave] {
            fcntl(fd, FcntlArg::F_SETFL(OFlag::O_NONBLOCK)).unwrap();
        }
        let host = AsyncFd::new(File::from(pair.slave)).unwrap();
        let user = AsyncFd::new(File::from(pair.master)).unwrap();
        write(&user, b"accept\n").await.unwrap();
        discard_pending_input(host.get_ref()).unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), read_line(&host))
                .await
                .is_err()
        );
        write(&user, b"cancel example\n").await.unwrap();
        assert_eq!(
            read_line(&host).await.unwrap().as_deref(),
            Some("cancel example")
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), read_line(&host))
                .await
                .is_err()
        );
    }

    #[test]
    fn only_explicit_current_valid_decisions_are_sent() {
        let schema = jsonschema::validator_for(&json!({"type":"object","properties":{"allow":{"type":"boolean"}},"required":["allow"],"additionalProperties":false})).unwrap();
        for input in [
            "",
            "yes",
            "accept old {\"allow\":true}",
            "accept current",
            "accept current {}",
            "accept current null",
            "accept current {\"allow\":\"yes\"}",
            "decline current {}",
            "cancel current {}",
        ] {
            assert!(
                decision(input, "current", &schema, false).is_err(),
                "{input}"
            );
        }
        let response = decision(
            "accept current {\"allow\":false}",
            "current",
            &schema,
            false,
        )
        .unwrap();
        assert_eq!(response.action, Action::Accept);
        assert_eq!(response.content, Some(json!({"allow":false})));
        assert_eq!(response.meta, None);
        assert_eq!(
            decision("decline current", "current", &schema, false)
                .unwrap()
                .action,
            Action::Decline
        );
        assert_eq!(
            decision("cancel current", "current", &schema, false).unwrap(),
            cancel()
        );
    }

    #[test]
    fn session_choice_is_explicit_and_requires_provider_scope() {
        let schema = jsonschema::validator_for(&json!({"type":"object"})).unwrap();
        assert!(decision("accept-session current {}", "current", &schema, false).is_err());
        assert_eq!(
            decision("accept current {}", "current", &schema, true)
                .unwrap()
                .meta,
            None
        );
        assert_eq!(
            decision("accept-session current {}", "current", &schema, true)
                .unwrap()
                .meta,
            Some(json!({"persist":"session"}))
        );
        let process = Arc::new(());
        let mut request = request();
        request.provider_session = Arc::downgrade(&process);
        request.context = Some(nanocodex_computer::ComputerElicitationContext {
            session_id: "conversation-a".into(),
            call_id: "call-a".into(),
            model: "fixture".into(),
        });
        request.params["_meta"] = json!({"persist":["session"],"connector_id":"computer-use","tool_name":"get_app_state","tool_params":{"app":"example.editor"},"progressToken":1,"tool_call_id":"call-a","x-codex-turn-metadata":{"call_id":"call-a","turn_id":"turn-a"}});
        let first = Scope::from_request(&request).unwrap();
        request.params["_meta"]["progressToken"] = json!(2);
        request.params["_meta"]["tool_call_id"] = json!("call-b");
        request.params["_meta"]["x-codex-turn-metadata"] =
            json!({"call_id":"call-b","turn_id":"turn-b"});
        request.context.as_mut().unwrap().call_id = "call-b".into();
        assert!(first.matches(&Scope::from_request(&request).unwrap()));
        for (field, value) in [
            ("connector_id", json!("other-provider")),
            ("tool_name", json!("click")),
            ("tool_params", json!({"app":"other.editor"})),
            ("riskLevel", json!("high")),
        ] {
            let mut different = request.clone();
            different.params["_meta"][field] = value;
            assert!(!first.matches(&Scope::from_request(&different).unwrap()));
        }
        let mut different = request.clone();
        different.context.as_mut().unwrap().session_id = "conversation-b".into();
        assert!(!first.matches(&Scope::from_request(&different).unwrap()));
        let next_process = Arc::new(());
        different = request.clone();
        different.provider_session = Arc::downgrade(&next_process);
        assert!(!first.matches(&Scope::from_request(&different).unwrap()));
        different = request.clone();
        different.params["_meta"]["persist"] = json!(["always"]);
        assert!(Scope::from_request(&different).is_none());
        different = request.clone();
        different.context = None;
        assert!(Scope::from_request(&different).is_none());
        drop(process);
        assert!(!first.matches(&first));
        assert!(Scope::from_request(&request).is_none());
    }

    #[test]
    fn review_escapes_terminal_and_unicode_controls() {
        let escaped = terminal_text("safe\n\x1b[2J\r\u{202e}\u{009b}");
        assert!(escaped.starts_with("safe\n"));
        assert!(!escaped.contains('\x1b'));
        assert!(!escaped.contains('\r'));
        assert!(!escaped.contains('\u{202e}'));
        assert!(!escaped.contains('\u{009b}'));
    }
}
