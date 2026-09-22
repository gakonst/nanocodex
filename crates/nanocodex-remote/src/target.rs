//! Checked publisher destinations and bounded, owner-private credential files.
//!
//! This value intentionally has no `Debug` implementation. Never put its bearer
//! in command arguments, URLs, logs, or externally visible error messages.
use std::{
    fs::OpenOptions,
    io::{self, Read},
    path::Path,
    sync::Arc,
};
use url::Url;

#[derive(Clone)]
pub struct PublisherTarget {
    endpoint: Url,
    bearer: Arc<str>,
}

impl PublisherTarget {
    /// Accept the existing standalone publisher origin/scoped-host contract.
    /// Account publishers use a bare origin; allocation publishers use their
    /// exact `/hands` endpoint. A credential rotation reloads this same file.
    pub fn from_credential_file(origin: &str, path: &Path) -> io::Result<Self> {
        let mut endpoint = checked_url(origin, false)?;
        if matches!(endpoint.path(), "" | "/") {
            endpoint.set_path("/v1/account/hands");
        } else if !scoped_path(endpoint.path(), "hands") {
            return Err(invalid("invalid scoped publisher endpoint"));
        }
        Ok(Self {
            endpoint,
            bearer: read_credential(path)?.into(),
        })
    }

    /// Derive the screen destination from a previously authorized tool host.
    /// This includes server Hands as well as account and VM Hands.
    pub fn from_attachment(endpoint: &str, bearer: &str) -> io::Result<Self> {
        let mut endpoint = checked_url(endpoint, true)?;
        if endpoint.path() != "/v1/account/tool-host" && !scoped_path(endpoint.path(), "tool-host")
        {
            return Err(invalid("invalid publisher attachment endpoint"));
        }
        let path = format!(
            "{}/hands",
            endpoint.path().strip_suffix("/tool-host").unwrap()
        );
        let scheme = if endpoint.scheme() == "wss" {
            "https"
        } else {
            "http"
        };
        endpoint
            .set_scheme(scheme)
            .map_err(|()| invalid("invalid publisher transport"))?;
        endpoint.set_path(&path);
        check_bearer(bearer)?;
        Ok(Self {
            endpoint,
            bearer: bearer.into(),
        })
    }

    pub fn endpoint(&self) -> &Url {
        &self.endpoint
    }
    pub fn bearer(&self) -> &str {
        &self.bearer
    }

    /// The matching transport-only target, for callers adapting the shared
    /// publisher to the existing Hand attachment lifecycle.
    pub fn attachment_endpoint(&self) -> Url {
        let mut endpoint = self.endpoint.clone();
        let path = format!(
            "{}/tool-host",
            endpoint.path().strip_suffix("/hands").unwrap()
        );
        let scheme = if endpoint.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        endpoint
            .set_scheme(scheme)
            .expect("validated publisher URL");
        endpoint.set_path(&path);
        endpoint
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn checked_url(raw: &str, websocket: bool) -> io::Result<Url> {
    // Reject noncanonical input before URL parsing can erase traversal segments.
    // Publisher routes are ASCII; encoded paths have no supported meaning.
    if raw.bytes().any(|b| b <= b' ' || b == b'\\' || b == b'%') {
        return Err(invalid("invalid publisher endpoint"));
    }
    let endpoint = Url::parse(raw).map_err(|_| invalid("invalid publisher endpoint"))?;
    let (secure, local) = if websocket {
        ("wss", "ws")
    } else {
        ("https", "http")
    };
    let loopback = endpoint.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || !(endpoint.scheme() == secure || endpoint.scheme() == local && loopback)
        || raw.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(invalid("invalid publisher endpoint"));
    }
    Ok(endpoint)
}

fn scoped_path(path: &str, suffix: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 6 || !parts[0].is_empty() || parts[1] != "v1" || parts[5] != suffix {
        return false;
    }
    let uuid = |value: &str| value.len() == 36 && uuid::Uuid::parse_str(value).is_ok();
    match parts[2] {
        "vm-host-attachments" => {
            parts[3].len() == 43
                && parts[3]
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                && uuid(parts[4])
        }
        "hand-hosts" => uuid(parts[3]) && uuid(parts[4]),
        _ => false,
    }
}

fn check_bearer(bearer: &str) -> io::Result<()> {
    if bearer.is_empty() || bearer.len() > 8192 || bearer.bytes().any(|b| b <= b' ' || b >= 127) {
        return Err(invalid("invalid publisher credential"));
    }
    Ok(())
}

fn read_credential(path: &Path) -> io::Result<String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not block on a FIFO or follow a substituted symlink.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| invalid("cannot open publisher credential file"))?;
    let metadata = file
        .metadata()
        .map_err(|_| invalid("invalid publisher credential file"))?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return Err(invalid("invalid publisher credential file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid("publisher credential file must be private"));
        }
    }
    let mut bytes = Vec::new();
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read publisher credential file"))?;
    if bytes.len() > 8192 {
        return Err(invalid("invalid publisher credential file"));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| invalid("invalid publisher credential"))?;
    let bearer = text.trim();
    check_bearer(bearer)?;
    Ok(bearer.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "11111111-1111-4111-8111-111111111111";
    fn credential(path: &Path, value: &str) {
        std::fs::write(path, value).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    #[test]
    fn account_vm_and_server_destinations_round_trip_without_credential_in_url() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("credential");
        credential(&file, "synthetic-token\n");
        for origin in [
            "http://127.0.0.1:4011".to_owned(),
            "https://managed.example".into(),
            format!(
                "https://managed.example/v1/vm-host-attachments/{}/{ID}/hands",
                "p".repeat(43)
            ),
            format!("https://managed.example/v1/hand-hosts/{ID}/{ID}/hands"),
        ] {
            let target = PublisherTarget::from_credential_file(&origin, &file).unwrap();
            assert_eq!(target.bearer(), "synthetic-token");
            assert!(!target.endpoint().as_str().contains("synthetic-token"));
            let again = PublisherTarget::from_attachment(
                target.attachment_endpoint().as_str(),
                target.bearer(),
            )
            .unwrap();
            assert_eq!(target.endpoint(), again.endpoint());
        }
    }
    #[test]
    fn malformed_and_unscoped_destinations_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("credential");
        credential(&file, "synthetic-token");
        for origin in [
            "http://managed.example",
            "https://user:password@managed.example",
            "https://managed.example?secret=x",
            "https://managed.example#x",
            "https://managed.example/v1/account/hands",
            "https://managed.example/../",
            "https://managed.example/%2e/",
            "https://managed.example/v1/hand-hosts/a/b/hands",
            "https://managed.example\\path",
        ] {
            assert!(PublisherTarget::from_credential_file(origin, &file).is_err());
        }
        assert!(
            PublisherTarget::from_attachment(
                "wss://managed.example/v1/account/tool-host",
                "one\ntwo"
            )
            .is_err()
        );
        assert!(
            PublisherTarget::from_attachment(
                "wss://managed.example/v1/unrelated/tool-host",
                "synthetic-token"
            )
            .is_err()
        );
    }
    #[test]
    fn invalid_files_fail_without_including_secret_or_path_in_errors() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("private-file-label");
        for value in [
            String::new(),
            "one\ntwo".into(),
            "x".repeat(8193),
            "secret\0value".into(),
        ] {
            credential(&file, &value);
            let error = PublisherTarget::from_credential_file("https://managed.example", &file)
                .err()
                .unwrap()
                .to_string();
            assert!(!error.contains("private-file-label"));
            assert!(!error.contains("secret\0value"));
        }
        assert!(
            PublisherTarget::from_credential_file("https://managed.example", temp.path()).is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn permissions_symlinks_and_atomic_rotation() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("credential");
        credential(&file, "first-token");
        let first =
            PublisherTarget::from_credential_file("https://managed.example", &file).unwrap();
        let next = temp.path().join("replacement");
        credential(&next, "second-token");
        std::fs::rename(next, &file).unwrap();
        assert_eq!(first.bearer(), "first-token");
        assert_eq!(
            PublisherTarget::from_credential_file("https://managed.example", &file)
                .unwrap()
                .bearer(),
            "second-token"
        );
        let link = temp.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(PublisherTarget::from_credential_file("https://managed.example", &link).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(PublisherTarget::from_credential_file("https://managed.example", &file).is_err());
        credential(&file, "");
        assert!(PublisherTarget::from_credential_file("https://managed.example", &file).is_err());
    }
}
