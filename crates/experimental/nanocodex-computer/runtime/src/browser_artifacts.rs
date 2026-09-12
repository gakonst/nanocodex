use crate::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static SEQUENCE: AtomicU64 = AtomicU64::new(1);
#[derive(Default)]
pub(super) struct Artifacts {
    pub directory: Option<PathBuf>,
    pub files: BTreeMap<String, PathBuf>,
}
impl Artifacts {
    pub fn directory(&mut self) -> Result<PathBuf> {
        if let Some(d) = &self.directory {
            return Ok(d.clone());
        }
        let parent = std::env::temp_dir();
        for _ in 0..100 {
            let id = SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = parent.join(format!("skyre-browser-{}-{stamp}-{id}", std::process::id()));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&path) {
                Ok(()) => {
                    self.directory = Some(path.clone());
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::action(
            "Cannot create private browser artifact directory",
        ))
    }
    pub fn export(&mut self, title: &str, extension: &str, bytes: &[u8]) -> Result<Value> {
        if self.files.len() >= 1024 {
            return Err(Error::action("Browser export registry limit exceeded"));
        }
        if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
            return Err(Error::action("Export is empty or exceeds 64 MiB"));
        }
        if !extension.chars().all(|c| c.is_ascii_alphanumeric()) || extension.len() > 10 {
            return Err(Error::invalid("Invalid export extension"));
        }
        let stem: String = title
            .chars()
            .take(100)
            .map(|c| {
                if c.is_alphanumeric() || ['-', '_'].contains(&c) {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let id = format!("export-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed));
        let path = self.directory()?.join(format!(
            "{}-{id}.{extension}",
            if stem.is_empty() {
                "ExportedContent"
            } else {
                &stem
            }
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path)?;
        if let Err(e) = file.write_all(bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&path);
            return Err(e.into());
        }
        self.files.insert(id.clone(), path.clone());
        Ok(json!({"id":id,"path":path,"bytes":bytes.len()}))
    }
}
