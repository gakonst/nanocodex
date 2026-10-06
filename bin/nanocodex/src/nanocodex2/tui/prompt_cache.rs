//! Account-scoped, bounded prompt history. All filesystem work runs off the UI task.
use std::path::PathBuf;

use nanocodex_managed::ManagedClient;

use super::session::RecentPrompt;

const MAX_PROMPTS: usize = 100;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const MAX_CACHE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct PromptCache {
    path: PathBuf,
}

impl PromptCache {
    /// Resolve paths only: constructing the UI must not perform filesystem I/O.
    pub(crate) fn for_client(client: &ManagedClient) -> Result<Self, String> {
        let account = nanocodex_cli_auth::default_account_file()
            .map_err(|_| "Could not resolve prompt history directory".to_owned())?;
        let parent = account
            .parent()
            .ok_or("Account path has no parent directory")?;
        Ok(Self {
            path: parent
                .join("prompt-history")
                .join(format!("{}.json", client.local_cache_namespace())),
        })
    }

    pub(crate) async fn merge(
        &self,
        prompts: Vec<RecentPrompt>,
    ) -> Result<Vec<RecentPrompt>, String> {
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || merge_file(&path, prompts))
            .await
            .map_err(|_| "Prompt history worker failed".to_owned())?
    }
}

/// Apply the same privacy and work limits to persisted and in-memory history.
pub(super) fn bounded(prompts: Vec<RecentPrompt>) -> Vec<RecentPrompt> {
    normalize(prompts)
        .map(|(prompts, _)| prompts)
        .unwrap_or_default()
}

fn normalize(mut prompts: Vec<RecentPrompt>) -> Result<(Vec<RecentPrompt>, Vec<u8>), String> {
    for prompt in &mut prompts {
        // Workspace is display metadata, never a path to reopen. Unix folder
        // names may contain non-UTF-8 bytes, which PathBuf cannot encode as JSON.
        if prompt.workspace.to_str().is_none() {
            prompt.workspace = PathBuf::from(prompt.workspace.to_string_lossy().into_owned());
        }
        if let Some(summary) = super::vault::receipt_summary(&prompt.text) {
            prompt.text = summary;
        }
    }
    prompts.retain(|prompt| {
        !prompt.session_id.trim().is_empty()
            && !prompt.text.trim().is_empty()
            && prompt.text.len() <= MAX_TEXT_BYTES
    });
    // Stable sorting keeps the merge order for equal timestamps.
    prompts.sort_by_key(|prompt| std::cmp::Reverse(prompt.recorded_at_unix_ms));
    let mut seen = std::collections::HashSet::new();
    let mut retained = Vec::new();
    let mut bytes = vec![b'['];
    for prompt in prompts {
        if !seen.insert((prompt.session_id.clone(), prompt.text.clone())) {
            continue;
        }
        let encoded = serde_json::to_vec(&prompt)
            .map_err(|_| "Could not encode prompt history".to_owned())?;
        let separator = usize::from(!retained.is_empty());
        if bytes.len() + separator + encoded.len() + 1 > MAX_CACHE_BYTES {
            break;
        }
        if separator != 0 {
            bytes.push(b',');
        }
        bytes.extend(encoded);
        retained.push(prompt);
        if retained.len() == MAX_PROMPTS {
            break;
        }
    }
    bytes.push(b']');
    Ok((retained, bytes))
}

#[cfg(not(unix))]
fn merge_file(_: &std::path::Path, _: Vec<RecentPrompt>) -> Result<Vec<RecentPrompt>, String> {
    Err("Private prompt history is not supported on this platform".to_owned())
}

#[cfg(unix)]
fn merge_file(
    path: &std::path::Path,
    prompts: Vec<RecentPrompt>,
) -> Result<Vec<RecentPrompt>, String> {
    unix::merge(path, prompts).map_err(|error| format!("Prompt history: {error}"))
}

#[cfg(unix)]
mod unix {
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::unix::fs::{DirBuilderExt, MetadataExt},
        path::Path,
        time::{Duration, Instant},
    };

    use nix::{
        errno::Errno,
        fcntl::{OFlag, openat, renameat},
        sys::stat::{Mode, mkdirat},
        unistd::{UnlinkatFlags, geteuid, unlinkat},
    };

    use super::{MAX_CACHE_BYTES, RecentPrompt, normalize};

    fn invalid(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    fn private_file(file: &File) -> io::Result<()> {
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != geteuid().as_raw()
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
        {
            return Err(invalid("cache file must be private, owned, and regular"));
        }
        Ok(())
    }

    // Account ancestors may contain system symlinks (macOS /var and /tmp).
    // Resolve those once; only the cache directory and its entries must reject
    // symlinks. Hold its directory handle throughout the locked transaction.
    fn directory(path: &Path) -> io::Result<File> {
        let account_parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| invalid("missing cache directory name"))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(account_parent)?;
        let account_parent = File::open(std::fs::canonicalize(account_parent)?)?;
        let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
        let fd = match openat(&account_parent, name, flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::ENOENT) => {
                match mkdirat(&account_parent, name, Mode::from_bits_truncate(0o700)) {
                    Ok(()) | Err(Errno::EEXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                openat(&account_parent, name, flags, Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        let directory = File::from(fd);
        let meta = directory.metadata()?;
        if meta.uid() != geteuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(invalid("cache directory must be private and owned"));
        }
        Ok(directory)
    }

    struct Temporary<'a> {
        directory: &'a File,
        name: String,
    }

    impl Drop for Temporary<'_> {
        fn drop(&mut self) {
            let _ = unlinkat(
                self.directory,
                self.name.as_str(),
                UnlinkatFlags::NoRemoveDir,
            );
        }
    }

    pub(super) fn merge(path: &Path, incoming: Vec<RecentPrompt>) -> io::Result<Vec<RecentPrompt>> {
        let parent = path
            .parent()
            .ok_or_else(|| invalid("missing cache directory"))?;
        let name = path
            .file_name()
            .ok_or_else(|| invalid("missing cache filename"))?;
        let directory = directory(parent)?;
        let lock_name = format!("{}.lock", name.to_string_lossy());
        let flags = OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
        let lock = File::from(openat(
            &directory,
            lock_name.as_str(),
            flags | OFlag::O_RDWR | OFlag::O_CREAT,
            Mode::from_bits_truncate(0o600),
        )?);
        private_file(&lock)?;
        // Keep the lock file permanently: unlinking it could create two lock domains.
        let started = Instant::now();
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) => {
                    let remaining = Duration::from_secs(1).saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(io::Error::new(io::ErrorKind::TimedOut, "cache is busy"));
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
        let mut prompts = match openat(&directory, name, flags | OFlag::O_RDONLY, Mode::empty()) {
            Ok(fd) => {
                let file = File::from(fd);
                private_file(&file)?;
                if file.metadata()?.len() > MAX_CACHE_BYTES as u64 {
                    return Err(invalid("cache exceeds its size limit; left unchanged"));
                }
                let mut bytes = Vec::new();
                file.take(MAX_CACHE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_CACHE_BYTES {
                    return Err(invalid("cache exceeds its size limit; left unchanged"));
                }
                serde_json::from_slice::<Vec<RecentPrompt>>(&bytes)
                    .map_err(|_| invalid("cache is corrupt; left unchanged"))?
            }
            Err(Errno::ENOENT) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        prompts.extend(incoming);
        let (prompts, bytes) = normalize(prompts).map_err(io::Error::other)?;
        let temporary_name = format!(".prompt-history-{}.tmp", uuid::Uuid::new_v4());
        let mut file = File::from(openat(
            &directory,
            temporary_name.as_str(),
            flags | OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL,
            Mode::from_bits_truncate(0o600),
        )?);
        // Only remove a temporary file after this process successfully created it.
        let temporary = Temporary {
            directory: &directory,
            name: temporary_name,
        };
        private_file(&file)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        renameat(&directory, temporary.name.as_str(), &directory, name)?;
        directory.sync_all()?;
        Ok(prompts)
    }
}
