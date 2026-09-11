//! Report application readiness to systemd only after remote registration.
pub(crate) fn ready() {
    #[cfg(target_os = "linux")]
    if let Some(path) = std::env::var_os("NOTIFY_SOCKET") {
        use std::os::{
            linux::net::SocketAddrExt,
            unix::{
                ffi::OsStrExt,
                net::{SocketAddr, UnixDatagram},
            },
        };
        let result = (|| -> std::io::Result<()> {
            let bytes = path.as_bytes();
            let address = if let Some(name) = bytes.strip_prefix(b"@") {
                SocketAddr::from_abstract_name(name)?
            } else {
                SocketAddr::from_pathname(path)?
            };
            UnixDatagram::unbound()?.send_to_addr(b"READY=1", &address)?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(%error, "Could not report service readiness");
        }
    }
}
