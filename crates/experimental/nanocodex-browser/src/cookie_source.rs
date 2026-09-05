//! Harness-owned cookie import from non-Chromium profile stores.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::env;

use rusqlite::{Connection, OpenFlags, backup::Backup};

use crate::{BrowserCookie, BrowserCookieSameSite, BrowserStorageState};

/// A Firefox profile whose persistent cookies can seed a private browser.
#[derive(Clone, Debug)]
pub struct FirefoxCookieSource {
    profile_directory: PathBuf,
}

impl FirefoxCookieSource {
    /// Locates Firefox's selected standard profile.
    ///
    /// # Errors
    ///
    /// Returns an error when Firefox has no standard profile with a cookie
    /// database on the current platform.
    pub fn standard() -> Result<Self, BrowserCookieSourceError> {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            Err(BrowserCookieSourceError::StandardProfileUnavailable { browser: "Firefox" })
        }

        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let home =
                env::var_os("HOME").ok_or(BrowserCookieSourceError::HomeDirectoryUnavailable)?;
            #[cfg(target_os = "macos")]
            let root = PathBuf::from(home).join("Library/Application Support/Firefox");
            #[cfg(target_os = "linux")]
            let root = PathBuf::from(home).join(".mozilla/firefox");
            let contents = std::fs::read_to_string(root.join("profiles.ini"))?;
            let profile_directory = selected_firefox_profile(&root, &contents).ok_or(
                BrowserCookieSourceError::StandardProfileUnavailable { browser: "Firefox" },
            )?;
            Self::new(profile_directory)
        }
    }

    /// Uses an explicit Firefox profile directory.
    ///
    /// # Errors
    ///
    /// Returns an error when `cookies.sqlite` is unavailable.
    pub fn new(profile_directory: impl Into<PathBuf>) -> Result<Self, BrowserCookieSourceError> {
        let profile_directory = profile_directory.into();
        let cookies = profile_directory.join("cookies.sqlite");
        if !cookies.is_file() {
            return Err(BrowserCookieSourceError::CookieStoreUnavailable { path: cookies });
        }
        Ok(Self { profile_directory })
    }

    /// Copies and loads persistent cookies without mutating or locking Firefox.
    ///
    /// # Errors
    ///
    /// Returns an error when the SQLite online backup or typed query fails.
    pub fn load(&self) -> Result<BrowserStorageState, BrowserCookieSourceError> {
        let source = Connection::open_with_flags(
            self.profile_directory.join("cookies.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let mut snapshot = Connection::open_in_memory()?;
        let backup = Backup::new(&source, &mut snapshot)?;
        backup.run_to_completion(64, Duration::from_millis(5), None)?;
        drop(backup);

        let now = chrono::Utc::now().timestamp();
        let mut statement = snapshot.prepare(
            "SELECT name, value, host, path, expiry, isSecure, isHttpOnly, sameSite
             FROM moz_cookies
             WHERE originAttributes = '' AND expiry > ?1",
        )?;
        let cookies = statement
            .query_map([now], |row| {
                let same_site = match row.get::<_, i64>(7)? {
                    0 => Some(BrowserCookieSameSite::None),
                    1 => Some(BrowserCookieSameSite::Lax),
                    2 => Some(BrowserCookieSameSite::Strict),
                    _ => None,
                };
                Ok(BrowserCookie {
                    name: row.get(0)?,
                    value: row.get(1)?,
                    domain: row.get(2)?,
                    path: row.get(3)?,
                    expires_epoch_seconds: Some(row.get::<_, i64>(4)? as f64),
                    secure: row.get::<_, i64>(5)? != 0,
                    http_only: row.get::<_, i64>(6)? != 0,
                    same_site,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BrowserStorageState {
            cookies,
            origins: Vec::new(),
        })
    }
}

/// A Safari/WebKit binary cookie store that can seed a private browser.
#[derive(Clone, Debug)]
pub struct SafariCookieSource {
    cookie_store: PathBuf,
}

impl SafariCookieSource {
    /// Locates Safari's standard sandboxed cookie store on macOS.
    ///
    /// # Errors
    ///
    /// Returns an error on other platforms or when the store is unavailable.
    #[cfg(not(target_os = "macos"))]
    pub const fn standard() -> Result<Self, BrowserCookieSourceError> {
        Err(BrowserCookieSourceError::StandardProfileUnavailable { browser: "Safari" })
    }

    /// Locates Safari's standard sandboxed cookie store on macOS.
    ///
    /// # Errors
    ///
    /// Returns an error when the store is unavailable.
    #[cfg(target_os = "macos")]
    pub fn standard() -> Result<Self, BrowserCookieSourceError> {
        let home = env::var_os("HOME").ok_or(BrowserCookieSourceError::HomeDirectoryUnavailable)?;
        let home = PathBuf::from(home);
        let candidates = [
            home.join(
                "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
            ),
            home.join("Library/Cookies/Cookies.binarycookies"),
        ];
        let path = candidates
            .into_iter()
            .find(|path| path.is_file())
            .ok_or(BrowserCookieSourceError::StandardProfileUnavailable { browser: "Safari" })?;
        Self::new(path)
    }

    /// Uses an explicit Safari/WebKit `.binarycookies` file.
    ///
    /// # Errors
    ///
    /// Returns an error when the file is unavailable.
    pub fn new(cookie_store: impl Into<PathBuf>) -> Result<Self, BrowserCookieSourceError> {
        let cookie_store = cookie_store.into();
        if !cookie_store.is_file() {
            return Err(BrowserCookieSourceError::CookieStoreUnavailable { path: cookie_store });
        }
        Ok(Self { cookie_store })
    }

    /// Loads unexpired Safari cookies into harness-owned browser state.
    ///
    /// # Errors
    ///
    /// Returns an error when macOS denies access or the bounded decoder rejects
    /// a malformed cookie file.
    pub fn load(&self) -> Result<BrowserStorageState, BrowserCookieSourceError> {
        let now = chrono::Utc::now().timestamp();
        let jar = safari_binarycookies::from_path(&self.cookie_store)?;
        let cookies = jar
            .cookies()
            .filter(|cookie| cookie.expires_unix() > now)
            .map(|cookie| BrowserCookie {
                name: cookie.name.clone(),
                value: cookie.value.clone(),
                domain: cookie.domain.clone(),
                path: cookie.path.clone(),
                expires_epoch_seconds: Some(cookie.expires_unix() as f64),
                http_only: cookie.is_http_only(),
                secure: cookie.is_secure(),
                same_site: None,
            })
            .collect();
        Ok(BrowserStorageState {
            cookies,
            origins: Vec::new(),
        })
    }
}

fn selected_firefox_profile(root: &Path, contents: &str) -> Option<PathBuf> {
    let mut sections = Vec::<(String, BTreeMap<String, String>)>::new();
    let mut current = None::<(String, BTreeMap<String, String>)>;
    for line in contents.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            current = Some((section.to_owned(), BTreeMap::new()));
        } else if let Some((key, value)) = line.split_once('=')
            && let Some((_, values)) = &mut current
        {
            values.insert(key.trim().to_owned(), value.trim().to_owned());
        }
    }
    if let Some(section) = current {
        sections.push(section);
    }

    let install_candidates = sections
        .iter()
        .filter(|(name, _)| name.starts_with("Install"))
        .filter_map(|(_, values)| values.get("Default").map(|path| root.join(path)));
    let profile_candidates = sections
        .iter()
        .filter(|(name, values)| {
            name.starts_with("Profile") && values.get("Default").is_some_and(|value| value == "1")
        })
        .chain(
            sections
                .iter()
                .filter(|(name, _)| name.starts_with("Profile")),
        )
        .filter_map(|(_, values)| {
            let path = values.get("Path")?;
            Some(
                if values.get("IsRelative").is_some_and(|value| value == "0") {
                    PathBuf::from(path)
                } else {
                    root.join(path)
                },
            )
        });
    install_candidates
        .chain(profile_candidates)
        .find(|profile| profile.join("cookies.sqlite").is_file())
}

/// Failures while discovering or loading a browser cookie source.
#[derive(Debug, thiserror::Error)]
pub enum BrowserCookieSourceError {
    #[error("the home directory is unavailable")]
    HomeDirectoryUnavailable,
    #[error("the standard {browser} cookie profile is unavailable")]
    StandardProfileUnavailable { browser: &'static str },
    #[error("browser cookie store is unavailable at {path}")]
    CookieStoreUnavailable { path: PathBuf },
    #[error("browser cookie store I/O failed")]
    Io(#[from] std::io::Error),
    #[error("Firefox cookie snapshot failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Safari cookie decode failed")]
    Safari(#[from] safari_binarycookies::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_source_snapshots_transferable_unexpired_cookies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cookies.sqlite");
        let database = Connection::open(&path).unwrap();
        database
            .execute_batch(
                "CREATE TABLE moz_cookies (
                    name TEXT NOT NULL,
                    value TEXT NOT NULL,
                    host TEXT NOT NULL,
                    path TEXT NOT NULL,
                    expiry INTEGER NOT NULL,
                    isSecure INTEGER NOT NULL,
                    isHttpOnly INTEGER NOT NULL,
                    sameSite INTEGER NOT NULL,
                    originAttributes TEXT NOT NULL
                );",
            )
            .unwrap();
        let expiry = chrono::Utc::now().timestamp() + 3_600;
        database
            .execute(
                "INSERT INTO moz_cookies VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, 2, '')",
                ("session", "test-value", ".example.com", "/", expiry),
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO moz_cookies VALUES ('expired', 'old', '.example.com', '/', 1, 0, 0, 0, '')",
                [],
            )
            .unwrap();
        database
            .execute(
                "INSERT INTO moz_cookies VALUES ('container', 'private', '.example.com', '/', ?1, 0, 0, 0, '^userContextId=1')",
                [expiry],
            )
            .unwrap();
        drop(database);

        let state = FirefoxCookieSource::new(directory.path())
            .unwrap()
            .load()
            .unwrap();

        assert_eq!(state.cookies.len(), 1);
        let cookie = &state.cookies[0];
        assert_eq!(cookie.name, "session");
        assert_eq!(cookie.value, "test-value");
        assert_eq!(cookie.domain, ".example.com");
        assert!(cookie.secure);
        assert!(cookie.http_only);
        assert_eq!(cookie.same_site, Some(BrowserCookieSameSite::Strict));
    }

    #[test]
    fn selected_firefox_profile_prefers_the_locked_install() {
        let directory = tempfile::tempdir().unwrap();
        let selected = directory.path().join("Profiles/selected.default-release");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::write(selected.join("cookies.sqlite"), []).unwrap();
        let profiles = "[Profile0]\nPath=Profiles/other.default\nDefault=1\n\
                        [InstallABC]\nDefault=Profiles/selected.default-release\nLocked=1\n";

        assert_eq!(
            selected_firefox_profile(directory.path(), profiles),
            Some(selected)
        );
    }

    #[test]
    fn selected_firefox_profile_supports_absolute_paths() {
        let root = tempfile::tempdir().unwrap();
        let profile = tempfile::tempdir().unwrap();
        std::fs::write(profile.path().join("cookies.sqlite"), []).unwrap();
        let profiles = format!(
            "[Profile0]\nPath={}\nIsRelative=0\nDefault=1\n",
            profile.path().display()
        );

        assert_eq!(
            selected_firefox_profile(root.path(), &profiles),
            Some(profile.path().to_owned())
        );
    }
}
