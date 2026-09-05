use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr},
    os::unix::{net::UnixStream, process::CommandExt as _},
    path::{Path, PathBuf},
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use thiserror::Error;
use tracing::{error, warn};

use crate::terminate_child_with_parent;

const SOCKET_TIMEOUT: Duration = Duration::from_secs(5);
const API_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_API_RESPONSE_BYTES: usize = 64 * 1024;

/// Failure to launch, configure, or communicate with gvproxy.
#[derive(Debug, Error)]
pub enum GvproxyError {
    /// The child exited before publishing its control sockets.
    #[error("gvproxy exited before creating its sockets: {0}")]
    EarlyExit(std::process::ExitStatus),

    /// A required socket did not become ready before the startup deadline.
    #[error("gvproxy did not create {path} within {timeout:?}")]
    SocketTimeout {
        /// Socket that remained unavailable.
        path: PathBuf,
        /// Enforced startup deadline.
        timeout: Duration,
    },

    /// A caller attempted to expose a guest port beyond host loopback.
    #[error("refusing to expose a VM port on non-loopback host address {0}")]
    NonLoopbackForward(SocketAddr),

    /// Port zero cannot identify a listener created outside gvproxy.
    #[error("host port zero cannot identify the resulting gvproxy listener")]
    UnspecifiedHostPort,

    /// The gvproxy services API returned a non-success response.
    #[error("gvproxy services API returned {status}: {body}")]
    Api {
        /// HTTP status line returned by gvproxy.
        status: String,
        /// Bounded response body returned by gvproxy.
        body: String,
    },

    /// The gvproxy services response was not valid HTTP.
    #[error("gvproxy services API returned an invalid HTTP response")]
    InvalidApiResponse,

    /// The gvproxy services response exceeded the fixed bound.
    #[error("gvproxy services API response exceeded {MAX_API_RESPONSE_BYTES} bytes")]
    ApiResponseTooLarge,

    /// A gvproxy request or response could not be encoded.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// Child-process, socket, or filesystem I/O failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One owned gvproxy process supplying a private network stack to one VM.
///
/// The caller supplies an exclusive state directory. The process creates its
/// vfkit-compatible unixgram socket and private services socket there. Dropping
/// this value terminates and reaps gvproxy.
pub struct Gvproxy {
    child: Child,
    network_socket: PathBuf,
    services_socket: PathBuf,
    log: PathBuf,
    started_at: Instant,
}

impl Gvproxy {
    /// Guest-visible address that the default gvproxy network translates to
    /// host loopback.
    ///
    /// [`Self::spawn`] deliberately starts the pinned gvproxy without a custom
    /// configuration, whose `192.168.127.0/24` topology reserves the final
    /// usable address for this host route.
    pub const HOST_IPV4: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 254);

    /// Starts gvproxy and waits for both of its local sockets to become ready.
    ///
    /// # Errors
    ///
    /// Returns an error when the state directory or log cannot be prepared,
    /// gvproxy cannot start, or its network and services sockets do not become
    /// ready before the startup deadline.
    pub fn spawn(binary: &Path, state_directory: &Path, log: &Path) -> Result<Self, GvproxyError> {
        Self::spawn_with_process_group(binary, state_directory, log, ProcessGroup::Inherited, None)
    }

    /// Starts gvproxy with host TCP connections restricted to one capture port.
    ///
    /// The wrapper must apply the restriction to itself and then replace itself
    /// with gvproxy. This keeps the network policy out of post-fork callbacks.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::spawn`].
    #[cfg(target_os = "linux")]
    pub fn spawn_capture_only(
        binary: &Path,
        state_directory: &Path,
        log: &Path,
        wrapper: &Path,
        port: u16,
    ) -> Result<Self, GvproxyError> {
        Self::spawn_with_process_group(
            binary,
            state_directory,
            log,
            ProcessGroup::Inherited,
            Some((wrapper, port)),
        )
    }

    /// Starts gvproxy in a new process group and waits for its sockets.
    ///
    /// This is useful when an application handles terminal interrupts itself
    /// and must keep owned VM work alive long enough to drain it. Dropping the
    /// returned owner still terminates and reaps gvproxy.
    ///
    /// # Errors
    ///
    /// Returns the errors documented by [`Self::spawn`].
    pub fn spawn_isolated(
        binary: &Path,
        state_directory: &Path,
        log: &Path,
    ) -> Result<Self, GvproxyError> {
        Self::spawn_with_process_group(binary, state_directory, log, ProcessGroup::Isolated, None)
    }

    fn spawn_with_process_group(
        binary: &Path,
        state_directory: &Path,
        log: &Path,
        process_group: ProcessGroup,
        capture_only: Option<(&Path, u16)>,
    ) -> Result<Self, GvproxyError> {
        fs::create_dir_all(state_directory)?;
        if let Some(parent) = log.parent() {
            fs::create_dir_all(parent)?;
        }
        let network_socket = state_directory.join("network.sock");
        let services_socket = state_directory.join("services.sock");
        remove_stale_socket(&network_socket)?;
        remove_stale_socket(&services_socket)?;

        let log_path = log.to_path_buf();
        let log = fs::File::create(log)?;
        let mut command = if let Some((wrapper, port)) = capture_only {
            let mut command = std::process::Command::new(wrapper);
            command
                .arg("--host-capture-only")
                .arg(port.to_string())
                .arg(binary);
            command
        } else {
            std::process::Command::new(binary)
        };
        command
            .arg("--listen-vfkit")
            .arg(format!("unixgram:{}", network_socket.display()))
            .arg("--services")
            .arg(format!("unix://{}", services_socket.display()))
            .arg("--ssh-port")
            .arg("-1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        if process_group == ProcessGroup::Isolated {
            command.process_group(0);
        }
        terminate_child_with_parent(&mut command);
        let mut child = command.spawn()?;

        let started_at = Instant::now();
        while !network_socket.exists() || !services_socket.exists() {
            if let Some(status) = child.try_wait()? {
                return Err(GvproxyError::EarlyExit(status));
            }
            if started_at.elapsed() >= SOCKET_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GvproxyError::SocketTimeout {
                    path: if network_socket.exists() {
                        services_socket
                    } else {
                        network_socket
                    },
                    timeout: SOCKET_TIMEOUT,
                });
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(Self {
            child,
            network_socket,
            services_socket,
            log: log_path,
            started_at,
        })
    }

    /// Returns the vfkit-compatible unixgram network socket.
    #[must_use]
    pub fn network_socket(&self) -> &Path {
        &self.network_socket
    }

    /// Forwards one loopback-only host TCP listener to a guest TCP listener.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-loopback or unspecified host endpoint, a
    /// services-socket failure, or a rejected gvproxy request.
    pub fn forward_tcp(&self, local: SocketAddr, remote: SocketAddr) -> Result<(), GvproxyError> {
        if !local.ip().is_loopback() {
            return Err(GvproxyError::NonLoopbackForward(local));
        }
        if local.port() == 0 {
            return Err(GvproxyError::UnspecifiedHostPort);
        }
        let body = serde_json::to_vec(&ExposeRequest {
            local,
            remote,
            protocol: "tcp",
        })?;
        services_request(&self.services_socket, "/services/forwarder/expose", &body)
    }

    /// Removes a previously configured loopback TCP forward.
    ///
    /// # Errors
    ///
    /// Returns an error when the services socket cannot be reached or gvproxy
    /// rejects the request.
    pub fn unforward_tcp(&self, local: SocketAddr) -> Result<(), GvproxyError> {
        if !local.ip().is_loopback() {
            return Err(GvproxyError::NonLoopbackForward(local));
        }
        if local.port() == 0 {
            return Err(GvproxyError::UnspecifiedHostPort);
        }
        let body = serde_json::to_vec(&UnexposeRequest {
            local,
            protocol: "tcp",
        })?;
        services_request(&self.services_socket, "/services/forwarder/unexpose", &body)
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProcessGroup {
    Inherited,
    Isolated,
}

impl Drop for Gvproxy {
    fn drop(&mut self) {
        let process_id = self.child.id();
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let age_ms = elapsed_millis(self.started_at);
                let message = format!(
                    "nanocodex owner: gvproxy process {process_id} exited before owner cleanup \
                     after {age_ms} ms: {status}"
                );
                if let Err(error) = append_owner_diagnostic(&self.log, &message) {
                    warn!(
                        process.id = process_id,
                        error.message = %error,
                        "failed to retain unexpected gvproxy exit in its log"
                    );
                }
                error!(
                    process.id = process_id,
                    process.status = %status,
                    process.age_ms = age_ms,
                    "gvproxy exited before its owner"
                );
            }
            Ok(None) => {
                if let Err(error) = self.child.kill() {
                    warn!(
                        process.id = process_id,
                        error.message = %error,
                        "failed to terminate owned gvproxy process"
                    );
                }
                if let Err(error) = self.child.wait() {
                    warn!(
                        process.id = process_id,
                        error.message = %error,
                        "failed to reap owned gvproxy process"
                    );
                }
            }
            Err(error) => {
                warn!(
                    process.id = process_id,
                    error.message = %error,
                    "failed to inspect owned gvproxy process before cleanup"
                );
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

fn append_owner_diagnostic(log: &Path, message: &str) -> io::Result<()> {
    let mut log = OpenOptions::new().append(true).open(log)?;
    writeln!(log, "{message}")
}

fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[derive(Serialize)]
struct ExposeRequest {
    local: SocketAddr,
    remote: SocketAddr,
    protocol: &'static str,
}

#[derive(Serialize)]
struct UnexposeRequest {
    local: SocketAddr,
    protocol: &'static str,
}

fn remove_stale_socket(path: &Path) -> Result<(), io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn services_request(socket: &Path, path: &str, body: &[u8]) -> Result<(), GvproxyError> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(API_TIMEOUT))?;
    stream.set_write_timeout(Some(API_TIMEOUT))?;
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()?;

    let mut response = Vec::new();
    stream
        .take(
            u64::try_from(MAX_API_RESPONSE_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut response)?;
    if response.len() > MAX_API_RESPONSE_BYTES {
        return Err(GvproxyError::ApiResponseTooLarge);
    }
    let response = String::from_utf8_lossy(&response);
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or(GvproxyError::InvalidApiResponse)?;
    let status = head
        .lines()
        .next()
        .ok_or(GvproxyError::InvalidApiResponse)?;
    if !status.starts_with("HTTP/1.1 200 ") && !status.starts_with("HTTP/1.0 200 ") {
        return Err(GvproxyError::Api {
            status: status.to_owned(),
            body: body.trim().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{Ipv4Addr, SocketAddrV4},
        os::unix::{fs::PermissionsExt as _, net::UnixListener},
        process::Command,
    };

    use nix::unistd::getpgrp;

    use super::*;

    #[test]
    fn default_network_exposes_host_loopback_at_a_stable_address() {
        assert_eq!(Gvproxy::HOST_IPV4, Ipv4Addr::new(192, 168, 127, 254));
    }

    #[test]
    fn caller_selects_inherited_or_isolated_process_group() {
        let inherited = recorded_process_group(Gvproxy::spawn);
        let isolated = recorded_process_group(Gvproxy::spawn_isolated);
        let parent_group = getpgrp().as_raw();

        assert_eq!(inherited.1, parent_group);
        assert_ne!(inherited.0, inherited.1);
        assert_eq!(isolated.0, isolated.1);
        assert_ne!(isolated.1, parent_group);
    }

    #[test]
    fn unexpected_exit_is_retained_in_the_owned_log() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("gvproxy.log");
        fs::write(&log, "gvproxy output\n").unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .spawn()
            .unwrap();
        let mut gvproxy = Gvproxy {
            child,
            network_socket: directory.path().join("network.sock"),
            services_socket: directory.path().join("services.sock"),
            log: log.clone(),
            started_at: Instant::now(),
        };
        assert_eq!(gvproxy.child.wait().unwrap().code(), Some(23));

        drop(gvproxy);

        let retained = fs::read_to_string(log).unwrap();
        assert!(retained.starts_with("gvproxy output\n"));
        assert!(retained.contains("exited before owner cleanup"));
        assert!(retained.contains("exit status: 23"));
    }

    fn recorded_process_group(
        spawn: fn(&Path, &Path, &Path) -> Result<Gvproxy, GvproxyError>,
    ) -> (i32, i32) {
        let directory = tempfile::tempdir().unwrap();
        let binary = directory.path().join("fake-gvproxy");
        let record = directory.path().join("process-group");
        fs::write(
            &binary,
            "#!/bin/sh\n\
             directory=$(CDPATH= cd -- \"$(dirname -- \"$0\")\" && pwd)\n\
             pid=$$\n\
             pgid=$(ps -o pgid= -p \"$pid\" | tr -d ' ')\n\
             printf '%s %s\\n' \"$pid\" \"$pgid\" > \"$directory/process-group\"\n\
             exit 7\n",
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();

        let result = spawn(
            &binary,
            &directory.path().join("state"),
            &directory.path().join("gvproxy.log"),
        );
        assert!(matches!(result, Err(GvproxyError::EarlyExit(_))));

        let values = fs::read_to_string(record)
            .unwrap()
            .split_whitespace()
            .map(|value| value.parse::<i32>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(values.len(), 2);
        (values[0], values[1])
    }

    #[test]
    fn refuses_non_loopback_forwards_before_contacting_gvproxy() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("gvproxy.log");
        fs::write(&log, "").unwrap();
        let proxy = Gvproxy {
            child: std::process::Command::new("/usr/bin/true").spawn().unwrap(),
            network_socket: PathBuf::new(),
            services_socket: PathBuf::new(),
            log,
            started_at: Instant::now(),
        };
        let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 9222));
        let remote = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 127, 2), 9222));

        assert!(matches!(
            proxy.forward_tcp(local, remote),
            Err(GvproxyError::NonLoopbackForward(address)) if address == local
        ));
        assert!(matches!(
            proxy.unforward_tcp(local),
            Err(GvproxyError::NonLoopbackForward(address)) if address == local
        ));
    }

    #[test]
    fn bounds_untrusted_services_api_responses() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("services.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let expected_request = b"POST /services/test HTTP/1.1\r\n\
                Host: localhost\r\n\
                Content-Type: application/json\r\n\
                Content-Length: 2\r\n\
                Connection: close\r\n\r\n\
                {}";
            let mut request = vec![0_u8; expected_request.len()];
            stream.read_exact(&mut request).unwrap();
            assert_eq!(request, expected_request);
            stream
                .write_all(&vec![b'x'; MAX_API_RESPONSE_BYTES + 1])
                .unwrap();
        });

        assert!(matches!(
            services_request(&socket, "/services/test", b"{}"),
            Err(GvproxyError::ApiResponseTooLarge)
        ));
        server.join().unwrap();
    }
}
