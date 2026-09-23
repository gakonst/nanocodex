use super::*;
use crate::ComputerTools;
use nanocodex_oai_api::tools::{Tool, ToolContext, ToolInput};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nanocodex-startup-cache-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn cache(&self) -> CatalogCache {
        CatalogCache::managed(&self.0, &self.0.join("hosts/version"), "verified fixture")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn catalog() -> Vec<ProviderTool> {
    serde_json::from_value(json!([
        {"name":"js", "description":"Exact upstream docs\n  whitespace", "inputSchema":{"type":"object","properties":{"opaque":{"type":"string"}}}, "outputSchema":{"type":"object"}, "annotations":{"readOnlyHint":false}},
        {"name":"turn_ended", "inputSchema":{"type":"object"}, "_meta":{"ui":{"visibility":[]},"future":"untouched"}}
    ])).unwrap()
}

#[test]
fn complete_fingerprint_detects_nested_edits_replacements_permissions_and_symlinks() {
    let fixture = Fixture::new();
    let nested = fixture.0.join("bundle/nested");
    fs::create_dir_all(&nested).unwrap();
    let file = nested.join("resource");
    fs::write(&file, "original").unwrap();
    let root = fixture.0.join("bundle");
    let first = fingerprint(&root).unwrap();
    assert_eq!(fingerprint(&root).as_ref(), Some(&first));
    let modified = fs::metadata(&file).unwrap().modified().unwrap();
    fs::write(&file, "modified").unwrap(); // same size
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let edited = fingerprint(&root).unwrap();
    assert_ne!(
        edited, first,
        "ctime must invalidate an edit with restored mtime"
    );
    let replacement = fixture.0.join("replacement");
    fs::copy(&file, &replacement).unwrap();
    fs::rename(replacement, &file).unwrap();
    let replaced = fingerprint(&root).unwrap();
    assert_ne!(replaced, edited);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o700)).unwrap();
    let permissions = fingerprint(&root).unwrap();
    assert_ne!(permissions, replaced);
    let link = root.join("link");
    std::os::unix::fs::symlink("nested/resource", &link).unwrap();
    let linked = fingerprint(&root).unwrap();
    assert_ne!(linked, permissions);
    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&fixture.0, &link).unwrap();
    assert!(
        fingerprint(&root).is_none(),
        "external symlinks must not be cached"
    );
    fs::remove_file(&link).unwrap();
    let external_hop = fixture.0.join("external-hop");
    std::os::unix::fs::symlink(&file, &external_hop).unwrap();
    std::os::unix::fs::symlink(&external_hop, &link).unwrap();
    assert_eq!(link.canonicalize().unwrap(), file.canonicalize().unwrap());
    assert!(
        fingerprint(&root).is_none(),
        "an external symlink hop must not be cached even when its final target is internal"
    );
}

#[test]
fn cache_is_bounded_private_and_ignores_symlink_substitution() {
    let fixture = Fixture::new();
    let path = fixture.0.join(".startup-cache/test.json");
    let value = json!({"fixture":"exact"});
    assert_eq!(write(&path, &value), Some(()));
    assert_eq!(read::<serde_json::Value>(&path), Some(value.clone()));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read::<serde_json::Value>(&path).is_none());
    fs::remove_file(&path).unwrap();
    let other = fixture.0.join("other.json");
    fs::write(&other, value.to_string()).unwrap();
    std::os::unix::fs::symlink(&other, &path).unwrap();
    assert!(read::<serde_json::Value>(&path).is_none());
    fs::remove_file(&path).unwrap();
    assert!(write(&path, &"x".repeat(MAX_CACHE_BYTES as usize)).is_none());
    fs::write(&path, " ".repeat(MAX_CACHE_BYTES as usize + 1)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(read::<serde_json::Value>(&path).is_none());
    assert!(fresh(100, 100));
    assert!(!fresh(100, 99));
    assert!(!fresh(100, 100 + MAX_AGE_SECS));
}

#[test]
fn catalog_preserves_all_metadata_and_invalidates_identity_configuration_age_and_invalid_schema() {
    let fixture = Fixture::new();
    let cache = fixture.cache();
    let config = ComputerConfig::new("/fixture/provider");
    cache.save(&config, &catalog());
    assert_eq!(cache.load(&config).unwrap(), catalog());
    let mut changed = config.clone();
    changed.args.push("changed".into());
    assert!(cache.load(&changed).is_none());
    changed = config.clone();
    changed
        .environment
        .insert("FIXTURE".into(), "changed".into());
    assert!(cache.load(&changed).is_none());
    changed = config.clone();
    changed.executable = "/fixture/new-provider".into();
    assert!(cache.load(&changed).is_none());
    assert!(
        CatalogCache::managed(
            &fixture.0,
            &fixture.0.join("hosts/next-version"),
            "verified fixture"
        )
        .load(&config)
        .is_none()
    );
    assert!(
        CatalogCache::managed(&fixture.0, &fixture.0.join("hosts/version"), "next bundle")
            .load(&config)
            .is_none()
    );
    for (format, saved_at, tools) in [
        (2, now(), catalog()),
        (1, now() - MAX_AGE_SECS, catalog()),
        (1, now() + 60, catalog()),
        (1, now(), vec![catalog()[0].clone(); 2]),
    ] {
        write(
            &cache.path,
            &CatalogRecord {
                format,
                saved_at,
                identity: cache.key(&config).unwrap(),
                tools,
            },
        )
        .unwrap();
        assert!(cache.load(&config).is_none());
    }
    fs::write(&cache.path, "not json").unwrap();
    assert!(cache.load(&config).is_none());
}

#[tokio::test]
async fn cached_registration_starts_no_process_and_live_mismatch_prevents_every_tool_call() {
    let fixture = Fixture::new();
    let live_catalog = fixture.0.join("catalog.json");
    let log = fixture.0.join("started");
    let mut config = ComputerConfig::new("/bin/sh");
    config.args = vec![
        "-c".into(),
        r#"
printf 'start\n' >> "$2"
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{}}}'
IFS= read -r initialized
IFS= read -r list
cat "$1"
while IFS= read -r call; do
  printf 'call\n' >> "$2"
  printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"content":[]}}'
done
"#
        .into(),
        "fixture".into(),
        live_catalog.clone().into_os_string(),
        log.clone().into_os_string(),
    ];
    let cache = fixture.cache();
    cache.save(&config, &catalog());
    config.catalog_cache = Some(cache.clone());
    let computer = ComputerTools::connect(config.clone()).await.unwrap();
    assert_eq!(computer.catalog(), catalog());
    assert!(
        !log.exists(),
        "a cache hit must not launch even a discovery process"
    );
    let mut changed = catalog();
    changed[0].description = Some("changed by provider".into());
    fs::write(
        &live_catalog,
        format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":changed}})
        ),
    )
    .unwrap();
    let error = computer
        .js()
        .execute(
            ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
            ToolContext::new("fixture", "one", "call", &[], 16000),
        )
        .await
        .err()
        .unwrap()
        .to_string();
    assert!(error.contains("catalog changed"), "{error}");
    assert_eq!(fs::read_to_string(&log).unwrap(), "start\n");
    assert!(
        cache.load(&config).is_none(),
        "reconnect must rediscover a changed live catalog"
    );
    fs::write(
        &live_catalog,
        format!(
            "{}\n",
            json!({"jsonrpc":"2.0","id":2,"result":{"tools":catalog()}})
        ),
    )
    .unwrap();
    let computer = ComputerTools::connect(config.clone()).await.unwrap();
    computer
        .js()
        .execute(
            ToolInput::Function(serde_json::value::to_raw_value(&json!({})).unwrap()),
            ToolContext::new("fixture", "two", "call", &[], 16000),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "start\nstart\nstart\ncall\n"
    );
    fs::write(&cache.path, "corrupt").unwrap();
    ComputerTools::connect(config.clone()).await.unwrap();
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "start\nstart\nstart\ncall\nstart\n"
    );
    assert_eq!(cache.load(&config).unwrap(), catalog());
}
