use super::{Bridge, Command};
use std::io;
use tokio::sync::mpsc;

/// Keeps terminal integration available where the Unix control transport is absent.
pub struct Server {
    pub bridge: Bridge,
    pub commands: mpsc::Receiver<Command>,
}

impl Server {
    pub fn enabled() -> bool {
        false
    }

    pub fn start(_backend: &str) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TUI control requires Unix sockets",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_transport_is_disabled_and_cannot_start() {
        assert!(!Server::enabled());
        assert_eq!(
            Server::start("managed").err().unwrap().kind(),
            io::ErrorKind::Unsupported
        );
    }
}
