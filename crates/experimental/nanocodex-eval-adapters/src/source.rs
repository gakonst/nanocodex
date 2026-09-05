use std::{
    fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use sha2::{Digest as _, Sha256};

#[derive(Clone, Debug)]
pub(crate) struct SourceStore {
    root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SourceError {
    #[error("benchmark source filesystem operation failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("benchmark source command failed: {0}")]
    Command(String),
    #[error("benchmark source archive is invalid: {0}")]
    Archive(String),
    #[error("retained benchmark source is stale: {0}")]
    Stale(String),
}

impl SourceStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    #[allow(dead_code)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[allow(dead_code)]
    pub(crate) fn git_checkout(
        &self,
        relative: &str,
        url: &str,
        revision: &str,
    ) -> Result<PathBuf, SourceError> {
        self.git_checkout_with_policy(relative, url, revision, false)
    }

    pub(crate) fn git_checkout_with_materialized_lfs(
        &self,
        relative: &str,
        url: &str,
        revision: &str,
    ) -> Result<PathBuf, SourceError> {
        self.git_checkout_with_policy(relative, url, revision, true)
    }

    fn git_checkout_with_policy(
        &self,
        relative: &str,
        url: &str,
        revision: &str,
        allow_materialized_lfs: bool,
    ) -> Result<PathBuf, SourceError> {
        let destination = self.root.join(relative);
        if destination.exists() {
            let head = command_text(
                Command::new("git")
                    .arg("-C")
                    .arg(&destination)
                    .args(["rev-parse", "HEAD"]),
            )?;
            if head.trim() != revision {
                return Err(SourceError::Stale(format!(
                    "{} is at {}, expected {revision}",
                    destination.display(),
                    head.trim()
                )));
            }
            let dirty = command_text(Command::new("git").arg("-C").arg(&destination).args([
                "status",
                "--porcelain=v1",
                "-z",
            ]))?;
            if !dirty.trim().is_empty()
                && !(allow_materialized_lfs && self.allowed_materialized_lfs(relative, &dirty)?)
            {
                return Err(SourceError::Stale(format!(
                    "{} has local changes",
                    destination.display()
                )));
            }
            return Ok(destination);
        }
        fs::create_dir_all(&self.root).map_err(|source| io_error(&self.root, source))?;
        let temporary = tempfile::Builder::new()
            .prefix(".source-")
            .tempdir_in(&self.root)
            .map_err(|source| io_error(&self.root, source))?;
        command_status(Command::new("git").arg("init").arg(temporary.path()))?;
        command_status(Command::new("git").arg("-C").arg(temporary.path()).args([
            "fetch",
            "--depth=1",
            url,
            revision,
        ]))?;
        command_status(
            Command::new("git")
                .arg("-C")
                .arg(temporary.path())
                .args(["checkout", "--detach", "FETCH_HEAD"])
                .env("GIT_LFS_SKIP_SMUDGE", "1"),
        )?;
        fs::rename(temporary.keep(), &destination)
            .map_err(|source| io_error(&destination, source))?;
        Ok(destination)
    }

    pub(crate) fn materialize_checkout_lfs_file(
        &self,
        checkout: &str,
        relative: &Path,
        revision: &str,
        dataset: &str,
    ) -> Result<String, SourceError> {
        let relative_text = relative.to_str().ok_or_else(|| {
            SourceError::Stale(format!("non-UTF-8 LFS path: {}", relative.display()))
        })?;
        let object = self.checkout_object(checkout, relative_text)?;
        let expected = object.sha256;
        let destination = self.root.join(checkout).join(relative);
        if destination.is_file() && validate_sha256(&destination, &expected).is_ok() {
            return Ok(expected);
        }
        if let Some(bytes) = object.git_bytes {
            fs::write(&destination, bytes).map_err(|source| io_error(&destination, source))?;
            validate_sha256(&destination, &expected)?;
            return Ok(expected);
        }
        if destination.exists() {
            let pointer = fs::read_to_string(&destination)
                .map_err(|source| io_error(&destination, source))?;
            if parse_lfs_sha256(&pointer).as_deref() != Some(expected.as_str()) {
                return Err(SourceError::Stale(format!(
                    "{} is neither the pinned LFS pointer nor its object",
                    destination.display()
                )));
            }
            fs::remove_file(&destination).map_err(|source| io_error(&destination, source))?;
        }
        let url = format!(
            "https://huggingface.co/datasets/{dataset}/resolve/{revision}/{}",
            encode_url_path(relative_text)
        );
        self.download(&format!("{checkout}/{relative_text}"), &url, &expected)?;
        Ok(expected)
    }

    fn allowed_materialized_lfs(&self, checkout: &str, status: &str) -> Result<bool, SourceError> {
        let records = status
            .split('\0')
            .filter(|record| !record.is_empty())
            .collect::<Vec<_>>();
        if records.is_empty() {
            return Ok(false);
        }
        for record in records {
            let (state, relative) = record.split_at(3.min(record.len()));
            if state != " M " {
                return Ok(false);
            }
            let expected = self.checkout_object(checkout, relative)?.sha256;
            if validate_sha256(&self.root.join(checkout).join(relative), &expected).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn checkout_object(
        &self,
        checkout: &str,
        relative: &str,
    ) -> Result<CheckoutObject, SourceError> {
        let bytes = command_bytes(
            Command::new("git")
                .arg("-C")
                .arg(self.root.join(checkout))
                .arg("show")
                .arg(format!("HEAD:{relative}")),
        )?;
        if let Ok(pointer) = std::str::from_utf8(&bytes)
            && let Some(sha256) = parse_lfs_sha256(pointer)
        {
            Ok(CheckoutObject {
                sha256,
                git_bytes: None,
            })
        } else {
            Ok(CheckoutObject {
                sha256: hex::encode(Sha256::digest(&bytes)),
                git_bytes: Some(bytes),
            })
        }
    }

    #[allow(dead_code)]
    pub(crate) fn download(
        &self,
        relative: &str,
        url: &str,
        expected_sha256: &str,
    ) -> Result<PathBuf, SourceError> {
        let destination = self.root.join(relative);
        if destination.is_file() {
            validate_sha256(&destination, expected_sha256)?;
            return Ok(destination);
        }
        let parent = destination.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        let temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| io_error(parent, source))?;
        command_status(
            Command::new("curl")
                .args([
                    "--fail",
                    "--location",
                    "--silent",
                    "--show-error",
                    "--output",
                ])
                .arg(temporary.path())
                .arg(url),
        )?;
        validate_sha256(temporary.path(), expected_sha256)?;
        temporary
            .persist(&destination)
            .map_err(|error| io_error(&destination, error.error))?;
        Ok(destination)
    }

    #[allow(dead_code)]
    pub(crate) fn write_verified(
        &self,
        relative: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, SourceError> {
        let destination = self.root.join(relative);
        if destination.is_file() {
            let retained =
                fs::read(&destination).map_err(|source| io_error(&destination, source))?;
            if retained != bytes {
                return Err(SourceError::Stale(format!(
                    "{} does not match the derived pinned content",
                    destination.display()
                )));
            }
            return Ok(destination);
        }
        let parent = destination.parent().unwrap_or(&self.root);
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(parent).map_err(|source| io_error(parent, source))?;
        temporary
            .write_all(bytes)
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|source| io_error(temporary.path(), source))?;
        temporary
            .persist(&destination)
            .map_err(|error| io_error(&destination, error.error))?;
        Ok(destination)
    }

    pub(crate) fn validate_file(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> Result<(), SourceError> {
        validate_sha256(path, expected_sha256)
    }

    pub(crate) fn extract_zip_member(
        &self,
        relative: &str,
        archive: &Path,
        member: &str,
        password: &str,
        expected_sha256: &str,
    ) -> Result<PathBuf, SourceError> {
        let destination = self.root.join(relative);
        if destination.is_file() {
            validate_sha256(&destination, expected_sha256)?;
            return Ok(destination);
        }
        let file = fs::File::open(archive).map_err(|source| io_error(archive, source))?;
        let mut archive_reader = zip::ZipArchive::new(file).map_err(|error| {
            SourceError::Archive(format!("failed to open {}: {error}", archive.display()))
        })?;
        let mut entry = archive_reader
            .by_name_decrypt(member, password.as_bytes())
            .map_err(|error| {
                SourceError::Archive(format!(
                    "failed to decrypt {member:?} from {}: {error}",
                    archive.display()
                ))
            })?;
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(|error| {
            SourceError::Archive(format!(
                "failed to extract {member:?} from {}: {error}",
                archive.display()
            ))
        })?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != expected_sha256 {
            return Err(SourceError::Stale(format!(
                "extracted {member} has digest {actual}, expected {expected_sha256}"
            )));
        }
        self.write_verified(relative, &bytes)
    }

    pub(crate) fn prepare_huggingface_archive(
        &self,
        repository: &str,
        filename: &str,
        revision: &str,
        destination_relative: &str,
    ) -> Result<PathBuf, SourceError> {
        let destination = self.root.join(destination_relative);
        let marker = destination.join(".nanocodex-source-revision");
        if destination.is_dir()
            && fs::read_to_string(&marker).is_ok_and(|value| value.trim() == revision)
        {
            return Ok(destination);
        }
        if destination.exists() {
            return Err(SourceError::Stale(format!(
                "{} does not carry expected revision {revision}",
                destination.display()
            )));
        }
        let download = self.root.join(format!("{destination_relative}-archive"));
        fs::create_dir_all(&download).map_err(|source| io_error(&download, source))?;
        let executable = if Command::new("hf")
            .arg("--help")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            "hf"
        } else if Command::new("huggingface-cli")
            .arg("--help")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            "huggingface-cli"
        } else {
            return Err(SourceError::Command(format!(
                "preparing {repository} requires the authenticated Hugging Face CLI"
            )));
        };
        command_status(
            Command::new(executable)
                .arg("download")
                .arg(repository)
                .arg(filename)
                .args(["--repo-type", "dataset", "--revision", revision])
                .arg("--local-dir")
                .arg(&download),
        )?;
        fs::create_dir_all(&destination).map_err(|source| io_error(&destination, source))?;
        let archive = download.join(filename);
        command_status(
            Command::new("tar")
                .args(["-xzf"])
                .arg(&archive)
                .arg("--directory")
                .arg(&destination),
        )?;
        fs::write(&marker, format!("{revision}\n")).map_err(|source| io_error(&marker, source))?;
        fs::remove_file(&archive).map_err(|source| io_error(&archive, source))?;
        Ok(destination)
    }
}

#[allow(dead_code)]
fn validate_sha256(path: &Path, expected: &str) -> Result<(), SourceError> {
    let bytes = fs::read(path).map_err(|source| io_error(path, source))?;
    let actual = hex::encode(Sha256::digest(bytes));
    if actual == expected {
        Ok(())
    } else {
        Err(SourceError::Stale(format!(
            "{} has digest {actual}, expected {expected}",
            path.display()
        )))
    }
}

#[allow(dead_code)]
fn command_status(command: &mut Command) -> Result<(), SourceError> {
    let rendered = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| SourceError::Command(format!("{rendered}: {error}")))?;
    ensure_success(rendered, output).map(drop)
}

#[allow(dead_code)]
fn command_text(command: &mut Command) -> Result<String, SourceError> {
    let rendered = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| SourceError::Command(format!("{rendered}: {error}")))?;
    let output = ensure_success(rendered, output)?;
    String::from_utf8(output.stdout).map_err(|error| SourceError::Command(error.to_string()))
}

fn command_bytes(command: &mut Command) -> Result<Vec<u8>, SourceError> {
    let rendered = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| SourceError::Command(format!("{rendered}: {error}")))?;
    Ok(ensure_success(rendered, output)?.stdout)
}

#[allow(dead_code)]
fn ensure_success(rendered: String, output: Output) -> Result<Output, SourceError> {
    if output.status.success() {
        Ok(output)
    } else {
        Err(SourceError::Command(format!(
            "{rendered} exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

#[allow(dead_code)]
fn io_error(path: &Path, source: std::io::Error) -> SourceError {
    SourceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

struct CheckoutObject {
    sha256: String,
    git_bytes: Option<Vec<u8>>,
}

fn parse_lfs_sha256(pointer: &str) -> Option<String> {
    if !pointer.starts_with("version https://git-lfs.github.com/spec/v1\n") {
        return None;
    }
    pointer.lines().find_map(|line| {
        let digest = line.strip_prefix("oid sha256:")?;
        (digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')))
        .then(|| digest.to_owned())
    })
}

fn encode_url_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(b"0123456789ABCDEF"[usize::from(byte >> 4)]));
            encoded.push(char::from(b"0123456789ABCDEF"[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use sha2::{Digest as _, Sha256};
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::{SourceStore, encode_url_path, parse_lfs_sha256};

    #[test]
    fn parses_only_canonical_git_lfs_pointers() {
        let digest = "a".repeat(64);
        let pointer =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{digest}\nsize 12\n");

        assert_eq!(parse_lfs_sha256(&pointer), Some(digest));
        assert_eq!(parse_lfs_sha256("ordinary file"), None);
    }

    #[test]
    fn encodes_asset_paths_without_hiding_directory_boundaries() {
        assert_eq!(encode_url_path("files/a b#.pdf"), "files/a%20b%23.pdf");
    }

    #[test]
    fn extracts_zip_members_without_a_host_unzip_executable() {
        let temporary = tempfile::tempdir().unwrap();
        let archive = temporary.path().join("dataset.zip");
        let contents = b"question,answer\nwhy,because\n";
        let mut writer = ZipWriter::new(fs::File::create(&archive).unwrap());
        writer
            .start_file("dataset/example.csv", SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents).unwrap();
        writer.finish().unwrap();
        let store = SourceStore::new(temporary.path().join("sources"));
        let expected = hex::encode(Sha256::digest(contents));

        let extracted = store
            .extract_zip_member(
                "derived/example.csv",
                &archive,
                "dataset/example.csv",
                "unused",
                &expected,
            )
            .unwrap();

        assert_eq!(fs::read(extracted).unwrap(), contents);
    }
}
