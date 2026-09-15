//! Process-owned tab-context files behind the private native-messaging channel.
use crate::{Error, Result};
use base64::Engine;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

const PREFIX: &str = "codexRuntime/tabContextAsset/";
const MAX_ASSETS: usize = 16;
const MAX_BYTES: u64 = 100 * 1024 * 1024;

struct Asset {
    path: PathBuf,
    bytes: u64,
    finished: bool,
}

pub(super) struct Assets {
    directory: PathBuf,
    entries: BTreeMap<String, Asset>,
    closed: bool,
}

fn required<'a>(params: &'a Value, key: &str) -> Result<&'a str> {
    params[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::new(1, format!("Missing required parameter: {key}")))
}

fn truncate_utf8(value: &str, limit: usize) -> &str {
    let mut end = limit.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].trim_end()
}

fn basename(name: &str) -> String {
    let component = name
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("tab-context.txt");
    let mapped: String = component
        .chars()
        .map(|c| if "\"%*:<>?|".contains(c) { '_' } else { c })
        .collect();
    let name = mapped.trim();
    if name.is_empty() {
        return "tab-context.txt".into();
    }
    if name.len() <= 218 {
        return name.into();
    }
    if let Some((stem, extension)) = name.rsplit_once('.')
        && !stem.is_empty()
        && !extension.is_empty()
        && extension.len() < 218
    {
        let stem = truncate_utf8(stem, 218 - extension.len() - 1);
        if !stem.is_empty() {
            return format!("{stem}.{extension}");
        }
    }
    let name = truncate_utf8(name, 218);
    if name.is_empty() {
        "tab-context.txt".into()
    } else {
        name.into()
    }
}

fn io_error(action: &str, error: std::io::Error) -> Error {
    Error::new(
        1,
        format!("Failed to {action} Chrome tab context asset: {error}"),
    )
}

impl Assets {
    pub(super) fn new(parent: &Path) -> Self {
        Self {
            directory: parent.join("tab-context-assets"),
            entries: BTreeMap::new(),
            closed: false,
        }
    }

    // This dispatcher is invoked only after the native socket ownership handshake.
    // Unknown methods remain the browser bridge's responsibility.
    pub(super) fn dispatch(&mut self, request: &Value) -> Option<Value> {
        let operation = request["method"].as_str()?.strip_prefix(PREFIX)?;
        if !["create", "appendChunk", "finish", "remove", "abort"].contains(&operation) {
            return None;
        }
        let params = &request["params"];
        let result = if self.closed {
            Err(Error::new(1, "Native asset channel is disconnected"))
        } else {
            match operation {
                "create" => self.create(params),
                "appendChunk" => self.append(params),
                "finish" => self.finish(params),
                "remove" | "abort" => self.remove(params),
                _ => unreachable!(),
            }
        };
        Some(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":request["id"],"result":result}),
            Err(error) => {
                json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":error.code,"message":error.message}})
            }
        })
    }

    fn create(&mut self, params: &Value) -> Result<Value> {
        let name = required(params, "fileName")?;
        if self.entries.len() >= MAX_ASSETS {
            return Err(Error::new(1, "Too many active Chrome tab context assets"));
        }
        // The native host already verified the socket parent. Keep its files in
        // that private namespace, without a shared global temporary directory.
        super::unix::private_directory(&self.directory)?;
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes)
            .map_err(|error| Error::new(1, format!("Asset identity generation failed: {error}")))?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let id = format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        );
        let path = self.directory.join(format!("{id}-{}", basename(name)));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| io_error("create", error))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| io_error("set permissions for", error))?;
        // A failed permission update deliberately does not insert ownership.
        self.entries.insert(
            id.clone(),
            Asset {
                path: path.clone(),
                bytes: 0,
                finished: false,
            },
        );
        Ok(json!({"assetId":id,"path":path.to_string_lossy()}))
    }

    fn append(&mut self, params: &Value) -> Result<Value> {
        let id = required(params, "assetId")?;
        let encoded = required(params, "dataBase64")?;
        let asset = self
            .entries
            .get_mut(id)
            .ok_or_else(|| Error::new(1, "Chrome tab context asset was not found"))?;
        if asset.finished {
            return Err(Error::new(
                1,
                "Chrome tab context asset is already finished",
            ));
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| {
                Error::new(
                    1,
                    format!("Invalid Chrome tab context asset chunk: {error}"),
                )
            })?;
        let total = asset.bytes.saturating_add(decoded.len() as u64);
        if total > MAX_BYTES {
            return Err(Error::new(1, "Chrome tab context asset is too large"));
        }
        let mut file = OpenOptions::new()
            .append(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&asset.path)
            .map_err(|error| io_error("open", error))?;
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect", error))?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::new(
                1,
                "Chrome tab context asset must remain an owned regular file",
            ));
        }
        file.write_all(&decoded)
            .map_err(|error| io_error("write", error))?;
        asset.bytes = total;
        Ok(json!({}))
    }

    fn finish(&mut self, params: &Value) -> Result<Value> {
        let id = required(params, "assetId")?;
        let asset = self
            .entries
            .get_mut(id)
            .ok_or_else(|| Error::new(1, "Chrome tab context asset was not found"))?;
        asset.finished = true;
        Ok(json!({"assetId":id,"path":asset.path.to_string_lossy()}))
    }

    fn remove(&mut self, params: &Value) -> Result<Value> {
        let id = required(params, "assetId")?;
        if let Some(asset) = self.entries.remove(id) {
            unlink(&asset.path)?;
        }
        Ok(json!({}))
    }

    pub(super) fn shutdown(&mut self) {
        // Input and bridge reads run independently. Retiring the owner before
        // draining prevents a concurrent final request recreating a leaked file.
        self.closed = true;
        for (_, asset) in std::mem::take(&mut self.entries) {
            let _ = unlink(&asset.path);
        }
    }
}

fn unlink(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("remove", error)),
    }
}

impl Drop for Assets {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn native_asset_result_serializes_a_non_utf8_path_without_panicking() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory
            .path()
            .join(std::ffi::OsStr::from_bytes(b"owned-\xff"));
        let mut owner = Assets::new(directory.path());
        // APFS rejects non-UTF8 filenames. Exercise the result contract with an
        // explicit path value; this does not claim such a file exists on macOS.
        owner.entries.insert(
            "owned".into(),
            Asset {
                path: parent.join("owned.txt"),
                bytes: 0,
                finished: false,
            },
        );
        let response = owner.dispatch(&json!({"id":1,"method":"codexRuntime/tabContextAsset/finish","params":{"assetId":"owned"}})).unwrap();
        assert!(
            response["result"]["path"]
                .as_str()
                .unwrap()
                .contains("owned-�")
        );
        let finished = owner.dispatch(&json!({"id":2,"method":"codexRuntime/tabContextAsset/finish","params":response["result"]})).unwrap();
        assert_eq!(response["result"], finished["result"]);
    }

    #[test]
    fn retirement_precedes_cleanup_and_rejects_late_native_requests() {
        let directory = tempfile::tempdir().unwrap();
        let mut owner = Assets::new(directory.path());
        let create = json!({"id":1,"method":"codexRuntime/tabContextAsset/create","params":{"fileName":"owned.txt"}});
        let response = owner.dispatch(&create).unwrap();
        let path = PathBuf::from(response["result"]["path"].as_str().unwrap());
        assert!(path.exists());
        owner.shutdown();
        assert!(!path.exists());
        let response = owner.dispatch(&create).unwrap();
        assert_eq!(response["error"]["code"], 1);
        assert_eq!(
            response["error"]["message"],
            "Native asset channel is disconnected"
        );
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 0);
        owner.shutdown();
    }
}
