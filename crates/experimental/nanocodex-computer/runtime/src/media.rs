//! Private, persistent result files for the Sky filepath/data_url contracts.
use crate::{Error, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{fs, io::Write, path::PathBuf};

#[derive(Default)]
pub struct MediaStore {
    directory: Option<PathBuf>,
    bytes: usize,
    sequence: u64,
}
impl MediaStore {
    pub fn write(&mut self, data: &str, mime: &str) -> Result<Value> {
        let extension = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            "audio/wav" => "wav",
            _ => return Err(Error::invalid("Unsupported result media type")),
        };
        if data.len() > 88 * 1024 * 1024 {
            return Err(Error::action("Result media exceeds 64 MiB"));
        }
        let bytes = STANDARD
            .decode(data)
            .map_err(|_| Error::action("Invalid result media encoding"))?;
        if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
            return Err(Error::action("Result media is empty or exceeds 64 MiB"));
        }
        if self.bytes.saturating_add(bytes.len()) > 512 * 1024 * 1024 || self.sequence >= 4096 {
            return Err(Error::action("Session result media storage limit reached"));
        }
        if self.directory.is_none() {
            let mut nonce = [0u8; 24];
            getrandom::fill(&mut nonce)
                .map_err(|_| Error::action("Cannot create result media identity"))?;
            let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = std::env::temp_dir().join(format!("skyre-media-{nonce}"));
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&path)?;
            self.directory = Some(path);
        }
        let path = self
            .directory
            .as_ref()
            .unwrap()
            .join(format!("result-{}.{}", self.sequence, extension));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&path)?;
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_all()) {
            let _ = fs::remove_file(&path);
            return Err(error.into());
        }
        self.sequence += 1;
        self.bytes += bytes.len();
        let url = url::Url::from_file_path(&path)
            .map_err(|_| Error::action("Invalid result media path"))?;
        Ok(
            json!({"filepath":path,"url":url.as_str(),"data_url":format!("data:{mime};base64,{data}")}),
        )
    }
}
