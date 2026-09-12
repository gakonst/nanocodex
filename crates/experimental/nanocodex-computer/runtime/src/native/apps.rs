//! Public bundle metadata discovery. Creating NSBundle never loads app code.
use crate::{Error, Result, native::App};
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSString};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub fn bundle(path: &Path) -> Option<Value> {
    let metadata = NSBundle::bundleWithPath(&NSString::from_str(path.to_str()?))?;
    let id = metadata.bundleIdentifier()?.to_string();
    let name = ["CFBundleDisplayName", "CFBundleName"]
        .into_iter()
        .find_map(|key| {
            metadata
                .objectForInfoDictionaryKey(&NSString::from_str(key))?
                .downcast::<NSString>()
                .ok()
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    Some(json!({"bundleIdentifier":id,"displayName":name,"appPath":path,"isRunning":false}))
}
/// Read the captured app's bundle metadata without loading its code. The
/// controller's identifier remains the final lookup candidate even if metadata
/// is absent; display/localized names are never substituted for CFBundleName.
pub(super) fn instructions(app: &App) -> Option<String> {
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(&app.path));
    let name = bundle.as_ref().and_then(|bundle| {
        bundle
            .objectForInfoDictionaryKey(&NSString::from_str("CFBundleName"))?
            .downcast::<NSString>()
            .ok()
            .map(|name| name.to_string())
    });
    let http = bundle.as_ref().is_some_and(|bundle| supports_http(bundle));
    super::super::instructions::for_app(&app.id, name.as_deref(), http)
}

fn supports_http(bundle: &NSBundle) -> bool {
    let Some(types) = bundle
        .objectForInfoDictionaryKey(&NSString::from_str("CFBundleURLTypes"))
        .and_then(|value| value.downcast::<NSArray>().ok())
    else {
        return false;
    };
    // The source casts the whole URL-types array to dictionaries first. A
    // malformed later member must not allow an earlier HTTP match to succeed.
    if !types.iter().all(|value| {
        value.downcast_ref::<NSDictionary>().is_some_and(|entry| {
            entry
                .allKeys()
                .iter()
                .all(|key| key.downcast_ref::<NSString>().is_some())
        })
    }) {
        return false;
    }
    let key = NSString::from_str("CFBundleURLSchemes");
    let http = NSString::from_str("http");
    types.iter().any(|value| {
        let Some(schemes) = value
            .downcast_ref::<NSDictionary>()
            .and_then(|entry| entry.objectForKey(&key))
            .and_then(|value| value.downcast::<NSArray>().ok())
        else {
            return false;
        };
        // Likewise, each scheme list must be entirely string-valued before
        // testing for exact "http". Neither "https" nor "HTTP" is a match.
        schemes
            .iter()
            .all(|value| value.downcast_ref::<NSString>().is_some())
            && schemes.iter().any(|value| {
                value
                    .downcast_ref::<NSString>()
                    .is_some_and(|value| value.isEqualToString(&http))
            })
    })
}

pub fn discover(roots: &[PathBuf], running: Vec<App>) -> Result<Value> {
    let mut found = BTreeMap::new();
    let mut pending = roots
        .iter()
        .cloned()
        .map(|path| (path, 0usize))
        .collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut entries = 0usize;
    while let Some((path, depth)) = pending.pop() {
        if Instant::now() > deadline || entries > 20_000 || found.len() > 5000 {
            return Err(Error::action(
                "Installed application discovery exceeded its bounds",
            ));
        }
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            if let Some(row) = bundle(&path) {
                found.insert(path, row);
            }
            continue;
        }
        if depth >= 6 {
            continue;
        }
        let directory = match std::fs::read_dir(&path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Error::action(format!(
                    "Cannot read application directory: {error}"
                )));
            }
        };
        for entry in directory {
            let entry = entry?;
            entries += 1;
            let ty = entry.file_type()?;
            if ty.is_dir()
                || (ty.is_symlink()
                    && entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("app")))
            {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
    for app in running {
        found.insert(PathBuf::from(&app.path),json!({"bundleIdentifier":app.id,"displayName":app.name,"appPath":app.path,"isRunning":true}));
    }
    let mut rows = found.into_values().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        a["displayName"]
            .as_str()
            .cmp(&b["displayName"].as_str())
            .then(a["appPath"].as_str().cmp(&b["appPath"].as_str()))
    });
    Ok(json!(rows))
}
