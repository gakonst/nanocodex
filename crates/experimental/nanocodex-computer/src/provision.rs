//! Install the unmodified, signed OpenAI desktop bundle as a private CUA runtime.
use std::path::PathBuf;

fn runtime_root() -> Result<PathBuf, String> {
    let base = std::env::var_os("NANOCODEX_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".nanocodex"))
        })
        .ok_or("HOME or NANOCODEX_DIR is required to install OpenAI CUA")?;
    let base = if base.is_absolute() {
        base
    } else {
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(base)
    };
    Ok(base.join("runtimes/openai-cua"))
}

/// The managed provider location. A broken selection remains discoverable so
/// callers surface its error instead of silently switching to another provider.
/// Linux guests reuse this receipt convention for a preinstalled upstream launcher;
/// runtime arguments and environment belong in the launcher.
pub fn managed_provider_path() -> Option<PathBuf> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return None;
    }
    let path = runtime_root().ok()?.join("provider.json");
    if std::fs::metadata(&path).ok()?.len() > 65536 {
        return None;
    }
    let receipt = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(config_from_receipt(&receipt).ok()?.executable)
}

/// Reuse the cached runtime by default; refresh explicitly fetches the official
/// current release. Never modify an app in /Applications or ~/Applications.
pub async fn provision_upstream(force_refresh: bool) -> Result<serde_json::Value, String> {
    #[cfg(target_os = "macos")]
    {
        let root = runtime_root()?;
        let mut applications = vec![PathBuf::from("/Applications")];
        if let Some(home) = std::env::var_os("HOME") {
            applications.push(PathBuf::from(home).join("Applications"));
        }
        let cancellation = mac::Cancellation::new();
        let mut commands = mac::System::new(cancellation.flag());
        tokio::task::spawn_blocking(move || {
            mac::provision(&root, &applications, force_refresh, &mut commands)
        })
        .await
        .map_err(|e| format!("OpenAI CUA installation task failed: {e}"))?
    }
    #[cfg(target_os = "windows")]
    {
        windows_provision(force_refresh).await
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = force_refresh;
        Ok(serde_json::json!({"status": "unsupported", "platform": std::env::consts::OS}))
    }
}

/// Install the official native-messaging bridge shipped with the selected
/// browser component. Browser stores still require the user to confirm the
/// extension installation.
pub async fn configure_browser_bridge() -> Result<serde_json::Value, String> {
    #[cfg(target_os = "macos")]
    {
        let root = runtime_root()?;
        tokio::task::spawn_blocking(move || mac::configure_browser(&root))
            .await
            .map_err(|e| format!("Browser bridge setup task failed: {e}"))?
    }
    #[cfg(not(target_os = "macos"))]
    Ok(serde_json::json!({"status":"unsupported","platform":std::env::consts::OS}))
}

/// Interpret the installer's bounded receipt without adding legacy companion arguments.
pub fn config_from_receipt(receipt: &serde_json::Value) -> Result<crate::ComputerConfig, String> {
    #[derive(serde::Deserialize)]
    struct Receipt {
        status: String,
        transport: String,
        executable: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        environment: std::collections::BTreeMap<String, String>,
        #[cfg(unix)]
        #[serde(default)]
        catalog_cache: Option<crate::startup_cache::CatalogCache>,
    }
    let receipt: Receipt = serde_json::from_value(receipt.clone()).map_err(|e| e.to_string())?;
    if receipt.status != "installed"
        || receipt.transport != "mcp"
        || !receipt.executable.is_absolute()
    {
        return Err("Invalid managed CUA installation receipt".into());
    }
    let mut config = crate::ComputerConfig::mcp(receipt.executable);
    config.args = receipt.args.into_iter().map(Into::into).collect();
    config.environment = receipt
        .environment
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    #[cfg(unix)]
    {
        config.catalog_cache = receipt.catalog_cache;
    }
    Ok(config)
}

#[cfg(any(target_os = "windows", test))]
async fn windows_provision(refresh: bool) -> Result<serde_json::Value, String> {
    use base64::Engine as _;
    let script: Vec<u8> = include_str!("provision_windows.ps1")
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(script);
    let child = tokio::process::Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-EncodedCommand",
            &encoded,
        ])
        .env(
            "NANOCODEX_UPSTREAM_REFRESH",
            if refresh { "1" } else { "0" },
        )
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(600),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "OpenAI Store installation timed out")?
    .map_err(|e| e.to_string())?;
    if !result.status.success() {
        return Err(format!(
            "OpenAI CUA setup failed: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    let mut receipt: serde_json::Value = serde_json::from_slice(&result.stdout)
        .map_err(|e| format!("Invalid OpenAI Store receipt: {e}"))?;
    config_from_receipt(&receipt)?;
    let root = runtime_root()?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    // Keep the Nanocodex host outside the byte-verified upstream resources tree.
    // Each receipt owns an immutable host file so updates preserve running hosts.
    let host = root.join(format!(
        "windows-sky-host-{}-{stamp}.mjs",
        std::process::id()
    ));
    std::fs::write(&host, include_bytes!("windows_sky_host.mjs")).map_err(|e| e.to_string())?;
    let args = receipt["args"]
        .as_array_mut()
        .ok_or("OpenAI CUA Windows receipt has no provider arguments")?;
    args.insert(
        0,
        serde_json::Value::String(host.to_string_lossy().into_owned()),
    );
    config_from_receipt(&receipt)?;
    let stage = root.join(format!("provider-{}-{stamp}.json", std::process::id()));
    std::fs::write(
        &stage,
        serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::rename(&stage, root.join("provider.json")).map_err(|e| e.to_string())?;
    Ok(receipt)
}

#[cfg(any(target_os = "macos", all(test, unix)))]
#[cfg_attr(all(test, not(target_os = "macos")), allow(dead_code))]
mod mac {
    use base64::Engine as _;
    use fs2::FileExt as _;
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        ffi::OsString,
        fs::{self, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        time::{SystemTime, UNIX_EPOCH},
    };

    const APPCAST: &str = "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
    const ARCHIVE_HOST: &str = "persistent.oaistatic.com";
    const ARCHIVE_PREFIX: &str = "/codex-app-prod/ChatGPT-darwin-";
    const MAX_APPCAST_BYTES: u64 = 2 * 1024 * 1024;
    const MAX_CENTRAL_BYTES: u64 = 16 * 1024 * 1024;
    const MAX_COMPONENT_BYTES: u64 = 384 * 1024 * 1024;
    const TEAM: &str = "2DC432GLL2";
    const BUNDLE: &str = "com.openai.codex";
    const REQUIREMENT: &str = "=identifier \"com.openai.codex\" and anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";
    const TEAM_REQUIREMENT: &str =
        "=anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";
    const APP: &str = "Codex.app";
    const RESOURCES: &str = "Contents/Resources";
    const MODULES: &str = "cua_node/lib/node_modules";
    const SKY: &str = "@oai/sky/Codex Computer Use.app";
    const ENTRY: &str = "@oai/cua-repl/bin/cua-repl.mjs";

    pub(super) trait Commands {
        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String>;
        fn check_cancelled(&self) -> Result<(), String> {
            Ok(())
        }
    }

    const CANCELLED: &str = "OpenAI CUA setup cancelled";

    pub(super) struct Cancellation(Arc<AtomicBool>);
    impl Cancellation {
        pub(super) fn new() -> Self {
            Self(Arc::new(AtomicBool::new(false)))
        }

        pub(super) fn flag(&self) -> Arc<AtomicBool> {
            self.0.clone()
        }
    }
    impl Drop for Cancellation {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    pub(super) struct System {
        cancelled: Arc<AtomicBool>,
    }
    impl System {
        pub(super) fn new(cancelled: Arc<AtomicBool>) -> Self {
            Self { cancelled }
        }
    }

    // Give each subprocess its own process group so cancelling setup also
    // terminates helpers spawned by curl, codesign, ditto, or PlistBuddy.
    struct OwnedCommand {
        child: std::process::Child,
        reaped: bool,
    }
    impl OwnedCommand {
        fn new(child: std::process::Child) -> Self {
            Self {
                child,
                reaped: false,
            }
        }

        fn kill(&mut self) {
            if !self.reaped {
                // The unreaped group leader keeps its PID reserved until this
                // final group signal, avoiding accidental PID reuse.
                unsafe {
                    libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
                }
                let _ = self.child.kill();
            }
        }

        fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
            let status = self.child.wait()?;
            self.reaped = true;
            Ok(status)
        }

        fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
            let status = self.child.try_wait()?;
            self.reaped = status.is_some();
            Ok(status)
        }
    }
    impl Drop for OwnedCommand {
        fn drop(&mut self) {
            if !self.reaped {
                self.kill();
                let _ = self.wait();
            }
        }
    }

    impl Commands for System {
        fn check_cancelled(&self) -> Result<(), String> {
            if self.cancelled.load(Ordering::Acquire) {
                Err(CANCELLED.into())
            } else {
                Ok(())
            }
        }

        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String> {
            use std::{io::Read, os::unix::process::CommandExt};

            self.check_cancelled()?;
            let mut child = OwnedCommand::new(
                std::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .process_group(0)
                    .spawn()
                    .map_err(|error| format!("{program}: {error}"))?,
            );
            let mut stdout = child
                .child
                .stdout
                .take()
                .ok_or("CUA command stdout unavailable")?;
            let mut stderr = child
                .child
                .stderr
                .take()
                .ok_or("CUA command stderr unavailable")?;
            let out = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).map(|_| bytes)
            });
            let err = std::thread::spawn(move || {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).map(|_| bytes)
            });
            let mut cancelled = false;
            let status = loop {
                if self.cancelled.load(Ordering::Acquire) {
                    child.kill();
                    cancelled = true;
                    break child.wait().map_err(|error| error.to_string())?;
                }
                // Do not reap while descendants still own pipes. Keeping the
                // leader alive keeps its process-group ID safe for cancellation.
                if out.is_finished()
                    && err.is_finished()
                    && let Some(status) = child.try_wait().map_err(|error| error.to_string())?
                {
                    break status;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            };
            let stdout = out
                .join()
                .map_err(|_| "CUA command output reader failed")?
                .map_err(|error| error.to_string())?;
            let stderr = err
                .join()
                .map_err(|_| "CUA command error reader failed")?
                .map_err(|error| error.to_string())?;
            if cancelled {
                return Err(CANCELLED.into());
            }
            if !status.success() {
                return Err(format!(
                    "{program} failed ({}): {}",
                    status,
                    String::from_utf8_lossy(&stderr).trim()
                ));
            }
            Ok(format!(
                "{}{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            ))
        }
    }

    fn args(values: &[&str], path: &Path) -> Vec<OsString> {
        values
            .iter()
            .map(OsString::from)
            .chain([path.as_os_str().to_owned()])
            .collect()
    }

    fn io<T>(result: std::io::Result<T>) -> Result<T, String> {
        result.map_err(|e| e.to_string())
    }

    fn nonce() -> String {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        format!(
            "{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
        text.lines().find_map(|line| line.strip_prefix(name))
    }

    fn valid_build(build: &str) -> bool {
        !build.is_empty()
            && build.len() <= 80
            && build
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte))
    }

    #[derive(Debug)]
    enum Seal {
        Hash([u8; 32]),
        Symlink(String),
    }

    fn plist_dict<'a>(dict: roxmltree::Node<'a, 'a>) -> Vec<(&'a str, roxmltree::Node<'a, 'a>)> {
        let elements: Vec<_> = dict
            .children()
            .filter(roxmltree::Node::is_element)
            .collect();
        elements
            .chunks_exact(2)
            .filter(|pair| pair[0].tag_name().name() == "key")
            .map(|pair| (pair[0].text().unwrap_or_default(), pair[1]))
            .collect()
    }

    fn signed_seals(app: &Path) -> Result<HashMap<String, Seal>, String> {
        let bytes = io(fs::read(app.join("Contents/_CodeSignature/CodeResources")))?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("OpenAI CodeResources manifest is unexpectedly large".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "OpenAI CodeResources is not UTF-8")?;
        let document = roxmltree::Document::parse_with_options(
            text,
            roxmltree::ParsingOptions {
                allow_dtd: true,
                ..Default::default()
            },
        )
        .map_err(|e| format!("Invalid OpenAI CodeResources: {e}"))?;
        let root = document
            .descendants()
            .find(|node| node.has_tag_name("dict"))
            .ok_or("OpenAI CodeResources has no root dictionary")?;
        let files = plist_dict(root)
            .into_iter()
            .find(|(key, _)| *key == "files2")
            .map(|(_, node)| node)
            .filter(|node| node.has_tag_name("dict"))
            .ok_or("OpenAI CodeResources has no files2 seals")?;
        let mut seals = HashMap::new();
        for (name, value) in plist_dict(files) {
            if name.is_empty()
                || name.starts_with('/')
                || name.split('/').any(|part| matches!(part, "" | "." | ".."))
                || !value.has_tag_name("dict")
            {
                return Err("OpenAI CodeResources contains an unsafe resource name".into());
            }
            let fields = plist_dict(value);
            let seal = if let Some((_, node)) = fields.iter().find(|(key, _)| *key == "symlink") {
                Seal::Symlink(node.text().ok_or("Invalid symlink seal")?.to_owned())
            } else if let Some(data) = fields
                .iter()
                .find(|(key, _)| *key == "hash2")
                .and_then(|(_, node)| node.text())
            {
                let compact: String = data.chars().filter(|c| !c.is_whitespace()).collect();
                let decoded = base64::engine::general_purpose::STANDARD
                    .decode(compact)
                    .map_err(|_| "Invalid SHA-256 seal")?;
                Seal::Hash(
                    decoded
                        .try_into()
                        .map_err(|_| "Invalid SHA-256 seal length")?,
                )
            } else {
                // Nested signed code is sealed by cdhash + requirement. Selected
                // executables are verified independently with codesign below.
                continue;
            };
            if seals.insert(name.to_owned(), seal).is_some() {
                return Err("OpenAI CodeResources contains duplicate resource seals".into());
            }
        }
        Ok(seals)
    }

    fn verify_tree(
        contents: &Path,
        relative: &Path,
        seals: &HashMap<String, Seal>,
    ) -> Result<(), String> {
        let path = contents.join(relative);
        let metadata = io(fs::symlink_metadata(&path))?;
        if metadata.is_dir() {
            for entry in io(fs::read_dir(path))? {
                let entry = io(entry)?;
                verify_tree(contents, &relative.join(entry.file_name()), seals)?;
            }
            return Ok(());
        }
        let name = relative
            .to_str()
            .ok_or("OpenAI resource path is not UTF-8")?
            .replace('\\', "/");
        match (seals.get(&name), metadata.file_type().is_symlink()) {
            (Some(Seal::Symlink(expected)), true)
                if io(fs::read_link(&path))? == Path::new(expected) =>
            {
                Ok(())
            }
            (Some(Seal::Hash(expected)), false) if metadata.is_file() => {
                let actual: [u8; 32] = Sha256::digest(io(fs::read(path))?).into();
                if &actual == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "OpenAI resource failed its signed SHA-256 seal: {name}"
                    ))
                }
            }
            _ => Err(format!(
                "OpenAI resource does not match its signed seal: {name}"
            )),
        }
    }

    fn verify_code(app: &Path, relative: &str, commands: &mut impl Commands) -> Result<(), String> {
        let path = app.join(relative);
        commands.run(
            "/usr/bin/codesign",
            &args(
                &[
                    "--verify",
                    "--strict",
                    "--test-requirement",
                    TEAM_REQUIREMENT,
                ],
                &path,
            ),
        )?;
        let identity = commands.run(
            "/usr/bin/codesign",
            &args(&["--display", "--verbose=4"], &path),
        )?;
        if field(&identity, "TeamIdentifier=") != Some(TEAM) {
            return Err(format!("{} is not signed by OpenAI", path.display()));
        }
        Ok(())
    }

    fn verify(app: &Path, commands: &mut impl Commands) -> Result<String, String> {
        commands.run(
            "/usr/bin/codesign",
            &args(
                &[
                    "--verify",
                    "--strict",
                    "--ignore-resources",
                    "--test-requirement",
                    REQUIREMENT,
                ],
                app,
            ),
        )?;
        let identity = commands.run(
            "/usr/bin/codesign",
            &args(&["--display", "--verbose=4"], app),
        )?;
        if field(&identity, "TeamIdentifier=") != Some(TEAM)
            || field(&identity, "Identifier=") != Some(BUNDLE)
        {
            return Err(format!(
                "{} is not the signed OpenAI Codex bundle",
                app.display()
            ));
        }
        let plist = app.join("Contents/Info.plist");
        let bundle = commands.run(
            "/usr/libexec/PlistBuddy",
            &args(&["-c", "Print :CFBundleIdentifier"], &plist),
        )?;
        if bundle.trim() != BUNDLE {
            return Err(format!(
                "{} has an unexpected bundle identifier",
                app.display()
            ));
        }
        let build = commands.run(
            "/usr/libexec/PlistBuddy",
            &args(&["-c", "Print :CFBundleVersion"], &plist),
        )?;
        let build = build.trim();
        if !valid_build(build) {
            return Err("OpenAI bundle has an invalid build identifier".into());
        }
        let resources = app.join(RESOURCES);
        for relative in [
            "codex".to_owned(),
            "cua_node/bin/node".into(),
            "cua_node/bin/node_repl".into(),
            format!("{MODULES}/{ENTRY}"),
            format!("{MODULES}/@oai/sky/package.json"),
            format!("{MODULES}/@oai/browser-desktop/package.json"),
            format!("{MODULES}/{SKY}/Contents/MacOS/SkyComputerUseService"),
            "plugins/openai-bundled/plugins/chrome/scripts/installManifest.mjs".into(),
            "plugins/openai-bundled/plugins/chrome/scripts/check-extension-installed.js".into(),
        ] {
            let path = resources.join(relative);
            if !path.is_file() {
                return Err(format!(
                    "OpenAI CUA runtime is incomplete: {}",
                    path.display()
                ));
            }
        }
        let architecture = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "x64"
        };
        let extension_host = format!(
            "plugins/openai-bundled/plugins/chrome/extension-host/macos/{architecture}/ChatGPT for Chrome"
        );
        if !resources.join(&extension_host).is_file() {
            return Err("OpenAI browser bridge is missing its native host".into());
        }
        let seals = signed_seals(app)?;
        for relative in [
            Path::new("Resources/codex"),
            Path::new("Resources/cua_node"),
            Path::new("Resources/plugins/openai-bundled/plugins/chrome"),
        ] {
            verify_tree(&app.join("Contents"), relative, &seals)?;
        }
        for relative in [
            "Contents/Resources/codex",
            "Contents/Resources/cua_node/bin/node",
            "Contents/Resources/cua_node/bin/node_repl",
            &format!("{RESOURCES}/{extension_host}"),
            &format!("{RESOURCES}/{MODULES}/{SKY}"),
        ] {
            verify_code(app, relative, commands)?;
        }
        Ok(build.to_owned())
    }

    #[derive(serde::Deserialize, serde::Serialize)]
    struct VerificationRecord {
        format: u32,
        verified_at: u64,
        fingerprint: String,
        build: String,
    }

    // Cache a successful signature verification against a fingerprint of the
    // entire sparse bundle. Any mutation, expiry, or explicit refresh falls
    // back to the complete signature and signed-resource checks above.
    fn verified_cached(
        root: &Path,
        app: &Path,
        commands: &mut impl Commands,
        refresh: bool,
    ) -> Result<(String, Option<String>), String> {
        use crate::startup_cache as cache;

        let path = root.join(".startup-cache/verification-v1.json");
        commands.check_cancelled()?;
        let before = cache::fingerprint(app);
        commands.check_cancelled()?;
        if !refresh
            && let Some(fingerprint) = &before
            && let Some(record) = cache::read::<VerificationRecord>(&path)
            && record.format == 1
            && valid_build(&record.build)
            && cache::fresh(record.verified_at, cache::now())
            && record.fingerprint == *fingerprint
        {
            return Ok((record.build, before));
        }
        let build = verify(app, commands)?;
        let after = cache::fingerprint(app);
        commands.check_cancelled()?;
        if before.is_some() && before != after {
            return Err("OpenAI CUA bundle changed during signature verification".into());
        }
        let verified_fingerprint = before.filter(|fingerprint| after.as_ref() == Some(fingerprint));
        if let Some(fingerprint) = &verified_fingerprint {
            let _ = cache::write(
                &path,
                &VerificationRecord {
                    format: 1,
                    verified_at: cache::now(),
                    fingerprint: fingerprint.clone(),
                    build: build.clone(),
                },
            );
        }
        Ok((build, verified_fingerprint))
    }

    fn quote(path: &Path) -> Result<String, String> {
        let text = path
            .to_str()
            .ok_or("OpenAI CUA paths must be valid UTF-8")?;
        // A colon changes Node's path-list meaning, even when shell-quoted.
        if text.contains(':') || text.contains('\n') || text.contains('\r') {
            return Err("OpenAI CUA paths cannot contain colons or newlines".into());
        }
        Ok(format!("'{}'", text.replace('\'', "'\"'\"'")))
    }

    fn launcher(version: &Path) -> Result<String, String> {
        let resources = version.join(APP).join(RESOURCES);
        let runtime = resources.join("cua_node");
        let modules = resources.join(MODULES);
        // These are the actual shipped node_repl and cua-repl environment
        // contracts. CODEX_BINARY_PATH is not supported by this upstream.
        // The official host enables Tab.ax with BROWSER_USE_TINYSKY_ENABLED;
        // high-level browser tab creation and lookup require this capability.
        Ok(format!(
            "#!/bin/sh\nset -eu\nexport CUA_REPL_NODE_REPL_PATH={}\nexport CUA_REPL_ENABLED_SURFACES=browser,computer\nexport BROWSER_USE_TINYSKY_ENABLED=1\nexport NODE_REPL_NODE_PATH={}\nexport NODE_REPL_NODE_MODULE_DIRS={}\nexport NODE_REPL_TRUSTED_CODE_PATHS={}\nexport CODEX_CLI_PATH={}\nexport SKY_CUA_SERVICE_PATH={}\nexport NODE_REPL_UNTRUSTED_ENV_ALLOWLIST=SKY_CUA_SERVICE_PATH\nexport PATH={}:\"$PATH\"\nexec {} {} \"$@\"\n",
            quote(&runtime.join("bin/node_repl"))?,
            quote(&runtime.join("bin/node"))?,
            quote(&modules)?,
            quote(&modules)?,
            quote(&resources.join("codex"))?,
            quote(&modules.join(SKY))?,
            quote(&runtime.join("bin"))?,
            quote(&runtime.join("bin/node"))?,
            quote(&modules.join(ENTRY))?,
        ))
    }

    fn cached(
        root: &Path,
        commands: &mut impl Commands,
        refresh: bool,
    ) -> Result<Option<serde_json::Value>, String> {
        let current = root.join("current");
        match fs::symlink_metadata(&current) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
            Ok(_) => {}
        }
        let validate = || -> Result<PathBuf, String> {
            let target = io(fs::read_link(&current))?;
            // Only a direct version selection created by this installer is valid.
            let parts: Vec<_> = target.components().collect();
            if parts.len() != 2
                || parts[0].as_os_str() != "versions"
                || !matches!(parts[1], std::path::Component::Normal(_))
            {
                return Err("current must select a managed version".into());
            }
            let version = root.join(target);
            for path in [&version, &version.join(APP)] {
                if !io(fs::symlink_metadata(path))?.is_dir() {
                    return Err("managed bundle must be a directory, not a symlink".into());
                }
            }
            Ok(version)
        };
        let result = validate().and_then(|version| {
            let (build, fingerprint) =
                verified_cached(root, &version.join(APP), commands, refresh)?;
            commands.check_cancelled()?;
            let host = ensure_host(root, &version, HOST_MODULES)?;
            commands.check_cancelled()?;
            publish_receipt(root, &host, &build, fingerprint.as_deref(), commands)
        });
        result.map(Some).map_err(|error| {
            if error == CANCELLED {
                error
            } else {
                format!("Managed OpenAI CUA runtime is damaged: {error}. Run `nanocodex computer setup --refresh` to replace it")
            }
        })
    }

    const HOST_MODULES: &[(&str, &str)] = &[
        (
            "openai-cua-app-server.mjs",
            include_str!("openai-cua-app-server.mjs"),
        ),
        (
            "openai-cua-native-host.mjs",
            include_str!("openai-cua-native-host.mjs"),
        ),
    ];

    fn host_launcher(
        root: &Path,
        version: &Path,
        host: &Path,
        hash: &str,
    ) -> Result<String, String> {
        Ok(format!(
            "#!/bin/sh\nset -eu\nexport NANOCODEX_CUA_NATIVE_APP={}\nexport NANOCODEX_CUA_NATIVE_PROVIDER={}\nexport NANOCODEX_CUA_NATIVE_STATE={}\nexec {} {} \"$@\"\n",
            quote(&version.join(APP))?,
            quote(&host.join("upstream-cua-provider"))?,
            quote(&root.join("host-state").join(hash))?,
            quote(&version.join(APP).join(RESOURCES).join("cua_node/bin/node"))?,
            quote(&host.join("openai-cua-native-host.mjs"))?,
        ))
    }

    // The signed bundle and generated host have independent lifetimes. Source
    // upgrades select new content-addressed assets without touching the bundle.
    fn ensure_host(
        root: &Path,
        version: &Path,
        modules: &[(&str, &str)],
    ) -> Result<PathBuf, String> {
        let direct = launcher(version)?;
        let mut digest = Sha256::new();
        for content in modules
            .iter()
            .flat_map(|(name, source)| [*name, *source])
            .chain([
                direct.as_str(),
                version.join(APP).to_str().ok_or("Invalid bundle path")?,
            ])
        {
            digest.update((content.len() as u64).to_le_bytes());
            digest.update(content.as_bytes());
        }
        // Include the wrapper template too; placeholders avoid a circular hash.
        digest.update(host_launcher(
            root,
            version,
            &root.join("hosts/HASH"),
            "HASH",
        )?);
        let hash: String = digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let host = root.join("hosts").join(&hash);
        let wrapper = host_launcher(root, version, &host, &hash)?;
        let assets: Vec<_> = modules
            .iter()
            .copied()
            .chain([
                ("upstream-cua-provider", direct.as_str()),
                ("cua-provider", wrapper.as_str()),
            ])
            .collect();
        let validate = || -> Result<(), String> {
            if !io(fs::symlink_metadata(&host))?.is_dir() {
                return Err("managed host must be a directory, not a symlink".into());
            }
            for (name, content) in &assets {
                let path = host.join(name);
                let metadata = io(fs::symlink_metadata(&path))?;
                if !metadata.is_file() || io(fs::read(&path))? != content.as_bytes() {
                    return Err(format!(
                        "managed host asset is modified: {}",
                        path.display()
                    ));
                }
                if name.ends_with("cua-provider") {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 {
                        return Err(format!(
                            "managed host launcher is not executable: {}",
                            path.display()
                        ));
                    }
                }
            }
            Ok(())
        };
        match fs::symlink_metadata(&host) {
            Ok(_) => {
                validate()?;
                return Ok(host);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
        io(fs::create_dir_all(root.join("hosts")))?;
        let stage = Staging {
            path: root.join("hosts").join(format!(".staging-{}", nonce())),
            cleanup: true,
        };
        io(fs::create_dir(&stage.path))?;
        for (name, content) in &assets {
            let path = stage.path.join(name);
            io(fs::write(&path, content))?;
            if name.ends_with("cua-provider") {
                use std::os::unix::fs::PermissionsExt;
                io(fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(0o755),
                ))?;
            }
        }
        if let Err(error) = fs::rename(&stage.path, &host) {
            // Another setup may have published this hash first. Never replace
            // its nonempty directory or repair modified assets in place.
            if fs::symlink_metadata(&host).is_err() {
                return Err(error.to_string());
            }
        }
        validate()?;
        Ok(host)
    }

    fn publish_receipt(
        root: &Path,
        host: &Path,
        build: &str,
        fingerprint: Option<&str>,
        commands: &impl Commands,
    ) -> Result<serde_json::Value, String> {
        commands.check_cancelled()?;
        let mut receipt = serde_json::json!({"status": "installed", "build": build,
            "executable": host.join("cua-provider"), "transport": "mcp", "args": [], "environment": {}});
        if let Some(fingerprint) = fingerprint {
            receipt["catalog_cache"] = serde_json::to_value(
                crate::startup_cache::CatalogCache::managed(root, host, fingerprint),
            )
            .map_err(|error| error.to_string())?;
        }
        if fs::symlink_metadata(root.join("provider.json"))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() <= 65536)
            && fs::read(root.join("provider.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
                .as_ref()
                == Some(&receipt)
        {
            return Ok(receipt);
        }
        let stage = root.join(format!(".provider-{}.json", nonce()));
        io(fs::write(
            &stage,
            serde_json::to_vec(&receipt).map_err(|e| e.to_string())?,
        ))?;
        let result = commands
            .check_cancelled()
            .and_then(|()| io(fs::rename(&stage, root.join("provider.json"))));
        if result.is_err() {
            let _ = fs::remove_file(&stage);
        }
        result?;
        Ok(receipt)
    }

    struct Staging {
        path: PathBuf,
        cleanup: bool,
    }
    impl Drop for Staging {
        fn drop(&mut self) {
            if self.cleanup {
                let _ = fs::remove_dir_all(&self.path);
            }
        }
    }

    #[derive(Clone, Debug)]
    struct Release {
        build: String,
        url: String,
        length: u64,
    }

    fn validate_archive_url(value: &str) -> Result<(), String> {
        let url = url::Url::parse(value).map_err(|_| "Invalid OpenAI archive URL")?;
        if url.scheme() != "https"
            || url.host_str() != Some(ARCHIVE_HOST)
            || !url.path().starts_with(ARCHIVE_PREFIX)
            || !url.path().ends_with(".zip")
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err("OpenAI appcast selected an unexpected archive URL".into());
        }
        Ok(())
    }

    fn latest_release(stage: &Path, commands: &mut impl Commands) -> Result<Release, String> {
        let appcast = stage.join("appcast.xml");
        commands.run(
            "/usr/bin/curl",
            &[
                "--disable".into(),
                "--fail".into(),
                "--location".into(),
                "--proto".into(),
                "=https".into(),
                "--proto-redir".into(),
                "=https".into(),
                "--show-error".into(),
                "--silent".into(),
                "--connect-timeout".into(),
                "30".into(),
                "--max-time".into(),
                "60".into(),
                "--max-filesize".into(),
                MAX_APPCAST_BYTES.to_string().into(),
                "--output".into(),
                appcast.as_os_str().to_owned(),
                APPCAST.into(),
            ],
        )?;
        let bytes = io(fs::read(&appcast))?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_APPCAST_BYTES {
            return Err("OpenAI appcast is empty or too large".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "OpenAI appcast is not UTF-8")?;
        let document =
            roxmltree::Document::parse(text).map_err(|e| format!("Invalid OpenAI appcast: {e}"))?;
        let mut releases = Vec::new();
        for item in document
            .descendants()
            .filter(|node| node.has_tag_name("item"))
        {
            let value = |name| {
                item.children()
                    .find(|node| node.has_tag_name(name))
                    .and_then(|node| node.text())
            };
            if value("hardwareRequirements") != Some("arm64") {
                continue;
            }
            let Some(build) = value("version") else {
                continue;
            };
            if build.is_empty() || !build.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let Some(enclosure) = item.children().find(|node| node.has_tag_name("enclosure"))
            else {
                continue;
            };
            let Some(url) = enclosure.attribute("url") else {
                continue;
            };
            let Ok(length) = enclosure
                .attribute("length")
                .unwrap_or_default()
                .parse::<u64>()
            else {
                continue;
            };
            validate_archive_url(url)?;
            releases.push((
                build.parse::<u64>().map_err(|_| "Invalid OpenAI build")?,
                Release {
                    build: build.to_owned(),
                    url: url.to_owned(),
                    length,
                },
            ));
        }
        let (_, mut release) = releases
            .into_iter()
            .max_by_key(|(build, _)| *build)
            .ok_or("OpenAI appcast has no compatible release")?;
        let arm = cfg!(target_arch = "aarch64")
            || commands
                .run(
                    "/usr/sbin/sysctl",
                    &["-n".into(), "hw.optional.arm64".into()],
                )
                .is_ok_and(|value| value.trim() == "1");
        if !arm {
            release.url = release.url.replace("darwin-arm64-", "darwin-x64-");
            release.length = 0;
            validate_archive_url(&release.url)?;
        }
        Ok(release)
    }

    fn content_range(headers: &[u8], start: u64, end: u64) -> Result<u64, String> {
        let text = std::str::from_utf8(headers).map_err(|_| "Invalid HTTP range headers")?;
        let prefix = format!("bytes {start}-{end}/");
        text.lines()
            .rev()
            .find_map(|line| {
                let (name, value) = line.trim().split_once(':')?;
                if !name.eq_ignore_ascii_case("content-range") {
                    return None;
                }
                value.trim().strip_prefix(&prefix)?.parse().ok()
            })
            .ok_or_else(|| "OpenAI archive did not honor an exact byte range".into())
    }

    fn fetch_range(
        stage: &Path,
        commands: &mut impl Commands,
        url: &str,
        start: u64,
        end: u64,
        label: &str,
    ) -> Result<(PathBuf, u64), String> {
        if end < start {
            return Err("Invalid OpenAI archive byte range".into());
        }
        let output = stage.join(format!("{label}.part"));
        let headers = stage.join(format!("{label}.headers"));
        commands.run(
            "/usr/bin/curl",
            &[
                "--disable".into(),
                "--fail".into(),
                "--location".into(),
                "--proto".into(),
                "=https".into(),
                "--proto-redir".into(),
                "=https".into(),
                "--show-error".into(),
                "--silent".into(),
                "--connect-timeout".into(),
                "30".into(),
                "--max-time".into(),
                "540".into(),
                "--range".into(),
                format!("{start}-{end}").into(),
                "--dump-header".into(),
                headers.as_os_str().to_owned(),
                "--output".into(),
                output.as_os_str().to_owned(),
                url.into(),
            ],
        )?;
        if io(fs::metadata(&output))?.len() != end - start + 1 {
            return Err("OpenAI archive returned the wrong byte count".into());
        }
        let total = content_range(&io(fs::read(headers))?, start, end)?;
        Ok((output, total))
    }

    fn le16(bytes: &[u8], offset: usize) -> Result<u16, String> {
        bytes
            .get(offset..offset + 2)
            .and_then(|value| value.try_into().ok())
            .map(u16::from_le_bytes)
            .ok_or("Truncated ZIP metadata".into())
    }
    fn le32(bytes: &[u8], offset: usize) -> Result<u32, String> {
        bytes
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or("Truncated ZIP metadata".into())
    }

    #[derive(Clone)]
    struct ZipEntry {
        name: String,
        local: u64,
        central: Vec<u8>,
    }

    fn directory_location(
        tail: &[u8],
        tail_start: u64,
        total: u64,
    ) -> Result<(u64, u64, usize), String> {
        let offset = tail
            .windows(4)
            .rposition(|bytes| bytes == b"PK\x05\x06")
            .ok_or("OpenAI archive has no ZIP directory")?;
        let eocd = &tail[offset..];
        if eocd.len() < 22 || offset + 22 + le16(eocd, 20)? as usize != tail.len() {
            return Err("Invalid ZIP end record".into());
        }
        if le16(eocd, 4)? != 0 || le16(eocd, 6)? != 0 || le16(eocd, 8)? != le16(eocd, 10)? {
            return Err("Multi-disk ZIP archives are unsupported".into());
        }
        let count = le16(eocd, 10)? as usize;
        let size = le32(eocd, 12)? as u64;
        let start = le32(eocd, 16)? as u64;
        if count == u16::MAX as usize
            || size == u32::MAX as u64
            || start == u32::MAX as u64
            || size > MAX_CENTRAL_BYTES
            || start
                .checked_add(size)
                .is_none_or(|end| end > tail_start + offset as u64 || end > total)
        {
            return Err("Unsupported or invalid ZIP directory".into());
        }
        Ok((start, size, count))
    }

    fn zip_entries(central: &[u8], expected: usize) -> Result<Vec<ZipEntry>, String> {
        let mut entries = Vec::with_capacity(expected);
        let mut offset = 0usize;
        while offset < central.len() {
            if central.get(offset..offset + 4) != Some(b"PK\x01\x02") {
                return Err("Invalid ZIP central directory entry".into());
            }
            let name_len = le16(central, offset + 28)? as usize;
            let extra_len = le16(central, offset + 30)? as usize;
            let comment_len = le16(central, offset + 32)? as usize;
            let length = 46usize
                .checked_add(name_len)
                .and_then(|v| v.checked_add(extra_len))
                .and_then(|v| v.checked_add(comment_len))
                .ok_or("Oversized ZIP entry")?;
            let record = central
                .get(offset..offset + length)
                .ok_or("Truncated ZIP central directory")?
                .to_vec();
            let name = String::from_utf8(record[46..46 + name_len].to_vec())
                .map_err(|_| "ZIP path is not UTF-8")?;
            if name.starts_with('/')
                || name.contains('\\')
                || name.split('/').any(|part| matches!(part, "." | ".."))
            {
                return Err("Unsafe path in OpenAI archive".into());
            }
            let local = le32(&record, 42)? as u64;
            entries.push(ZipEntry {
                name,
                local,
                central: record,
            });
            offset += length;
        }
        if entries.len() != expected {
            return Err("ZIP entry count mismatch".into());
        }
        entries.sort_by_key(|entry| entry.local);
        if entries
            .windows(2)
            .any(|pair| pair[0].local >= pair[1].local)
        {
            return Err("Invalid ZIP local entry offsets".into());
        }
        Ok(entries)
    }

    fn selected_name(name: &str, prefix: &str) -> bool {
        name == format!("{prefix}Contents/Info.plist")
            || name.starts_with(&format!("{prefix}Contents/MacOS/"))
            || name == format!("{prefix}Contents/_CodeSignature/CodeResources")
            || name == format!("{prefix}{RESOURCES}/codex")
            || name.starts_with(&format!("{prefix}{RESOURCES}/cua_node/"))
            || name.starts_with(&format!(
                "{prefix}{RESOURCES}/plugins/openai-bundled/plugins/chrome/"
            ))
    }

    fn component_zip(
        stage: &Path,
        commands: &mut impl Commands,
        release: &Release,
    ) -> Result<PathBuf, String> {
        let (_, total) = fetch_range(stage, commands, &release.url, 0, 0, "probe")?;
        if release.length != 0 && release.length != total {
            return Err("OpenAI appcast archive length changed".into());
        }
        if total < 22 {
            return Err("OpenAI archive is too small".into());
        }
        let tail_size = total.min(65_557);
        let tail_start = total - tail_size;
        let (tail_path, tail_total) =
            fetch_range(stage, commands, &release.url, tail_start, total - 1, "tail")?;
        if tail_total != total {
            return Err("OpenAI archive changed during download".into());
        }
        let tail = io(fs::read(tail_path))?;
        let (central_start, central_size, count) = directory_location(&tail, tail_start, total)?;
        let (central_path, central_total) = fetch_range(
            stage,
            commands,
            &release.url,
            central_start,
            central_start + central_size - 1,
            "central",
        )?;
        if central_total != total {
            return Err("OpenAI archive changed during download".into());
        }
        let entries = zip_entries(&io(fs::read(central_path))?, count)?;
        let info = entries
            .iter()
            .find(|entry| {
                entry.name.ends_with(".app/Contents/Info.plist")
                    && !entry.name[..entry.name.len() - ".app/Contents/Info.plist".len()]
                        .contains('/')
            })
            .ok_or("OpenAI archive has no top-level app bundle")?;
        let prefix = info
            .name
            .strip_suffix("Contents/Info.plist")
            .unwrap()
            .to_owned();
        if prefix != "ChatGPT.app/" && prefix != "Codex.app/" {
            return Err("OpenAI archive has an unexpected app bundle".into());
        }
        let chosen: Vec<bool> = entries
            .iter()
            .map(|entry| selected_name(&entry.name, &prefix))
            .collect();
        if chosen.iter().filter(|value| **value).count() < 8 {
            return Err("OpenAI archive is missing CUA components".into());
        }
        let mut groups = Vec::<(usize, usize, u64, u64)>::new();
        for (index, selected) in chosen.iter().enumerate() {
            if !selected {
                continue;
            }
            let end = entries
                .get(index + 1)
                .map_or(central_start, |entry| entry.local);
            if end <= entries[index].local || end > central_start {
                return Err("Invalid ZIP entry span".into());
            }
            if let Some(group) = groups.last_mut().filter(|group| group.1 + 1 == index) {
                group.1 = index;
                group.3 = end;
            } else {
                groups.push((index, index, entries[index].local, end));
            }
        }
        let component_bytes: u64 = groups.iter().map(|group| group.3 - group.2).sum();
        if component_bytes > MAX_COMPONENT_BYTES {
            return Err("OpenAI CUA components exceed the download limit".into());
        }
        let archive = stage.join("components.zip");
        let mut output = io(fs::File::create(&archive))?;
        let mut offsets = HashMap::new();
        let mut written = 0u64;
        for (number, (first, last, start, end)) in groups.iter().copied().enumerate() {
            let (part, part_total) = fetch_range(
                stage,
                commands,
                &release.url,
                start,
                end - 1,
                &format!("payload-{number}"),
            )?;
            if part_total != total {
                return Err("OpenAI archive changed during download".into());
            }
            for entry in &entries[first..=last] {
                offsets.insert(entry.local, written + entry.local - start);
            }
            let mut input = io(fs::File::open(part))?;
            written += io(std::io::copy(&mut input, &mut output))?;
        }
        let central_offset = written;
        let mut selected_count = 0u16;
        for entry in entries
            .iter()
            .filter(|entry| selected_name(&entry.name, &prefix))
        {
            let mut record = entry.central.clone();
            let offset: u32 = (*offsets
                .get(&entry.local)
                .ok_or("Missing ZIP component offset")?)
            .try_into()
            .map_err(|_| "Component ZIP is too large")?;
            record[42..46].copy_from_slice(&offset.to_le_bytes());
            io(output.write_all(&record))?;
            written += record.len() as u64;
            selected_count = selected_count
                .checked_add(1)
                .ok_or("Too many component ZIP entries")?;
        }
        let central_length: u32 = (written - central_offset)
            .try_into()
            .map_err(|_| "Component ZIP directory is too large")?;
        let central_offset: u32 = central_offset
            .try_into()
            .map_err(|_| "Component ZIP is too large")?;
        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(b"PK\x05\x06");
        eocd.extend_from_slice(&0u16.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        eocd.extend_from_slice(&selected_count.to_le_bytes());
        eocd.extend_from_slice(&selected_count.to_le_bytes());
        eocd.extend_from_slice(&central_length.to_le_bytes());
        eocd.extend_from_slice(&central_offset.to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes());
        io(output.write_all(&eocd))?;
        Ok(archive)
    }

    fn download(
        stage: &mut Staging,
        commands: &mut impl Commands,
        release: &Release,
    ) -> Result<PathBuf, String> {
        let archive = component_zip(&stage.path, commands, release)?;
        let unpacked = stage.path.join("unpacked");
        io(fs::create_dir(&unpacked))?;
        commands.run(
            "/usr/bin/ditto",
            &[
                "-x".into(),
                "-k".into(),
                archive.into_os_string(),
                unpacked.as_os_str().to_owned(),
            ],
        )?;
        let source = ["ChatGPT.app", "Codex.app"]
            .into_iter()
            .map(|name| unpacked.join(name))
            .find(|path| path.is_dir())
            .ok_or("Official OpenAI component archive contains no supported app bundle")?;
        let build = verify(&source, commands)?;
        if build != release.build {
            return Err("OpenAI appcast build does not match its signed bundle".into());
        }
        let destination = stage.path.join("payload").join(APP);
        io(fs::rename(source, &destination))?;
        Ok(destination)
    }

    pub(super) fn provision(
        root: &Path,
        _applications: &[PathBuf],
        refresh: bool,
        commands: &mut impl Commands,
    ) -> Result<serde_json::Value, String> {
        // Every CLI and the persistent Hand can discover CUA at the same time.
        // Serialize the expensive download and re-check the cache only after
        // acquiring the lock so concurrent first use publishes one runtime.
        commands.check_cancelled()?;
        io(fs::create_dir_all(root))?;
        let lock = io(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("provision.lock")))?;
        io(lock.lock_exclusive())?;
        commands.check_cancelled()?;
        if !refresh && let Some(receipt) = cached(root, commands, false)? {
            return Ok(receipt);
        }
        io(fs::create_dir_all(root.join("versions")))?;
        let mut stage = Staging {
            path: root.join(format!(".staging-{}", nonce())),
            cleanup: true,
        };
        io(fs::create_dir(&stage.path))?;
        io(fs::create_dir(stage.path.join("payload")))?;
        let release = latest_release(&stage.path, commands)?;
        if refresh {
            match cached(root, commands, true) {
                Ok(Some(existing)) if existing["build"].as_str() == Some(&release.build) => {
                    return Ok(existing);
                }
                Err(error) if error == CANCELLED => return Err(error),
                _ => {}
            }
        }
        let app = download(&mut stage, commands, &release)?;
        let build = verify(&app, commands)?;
        commands.check_cancelled()?;
        let relative = PathBuf::from("versions").join(format!("{build}-{}", nonce()));
        let version = root.join(&relative);
        io(fs::rename(stage.path.join("payload"), &version))?;
        // Finish the host before changing the selected bundle. Failed host
        // preparation must leave the previous selection and receipt intact.
        let host = ensure_host(root, &version, HOST_MODULES)?;
        // Publication is a single rename. Previous versions remain available to
        // processes already using their absolute bundle paths.
        let next = stage.path.join("next");
        #[cfg(unix)]
        io(std::os::unix::fs::symlink(&relative, &next))?;
        #[cfg(not(unix))]
        return Err("macOS CUA publication requires Unix symlinks".into());
        let fingerprint = crate::startup_cache::fingerprint(&version.join(APP));
        commands.check_cancelled()?;
        io(fs::rename(&next, root.join("current")))?;
        publish_receipt(root, &host, &build, fingerprint.as_deref(), commands)
    }

    #[cfg(target_os = "macos")]
    pub(super) fn configure_browser(root: &Path) -> Result<serde_json::Value, String> {
        let target = io(fs::read_link(root.join("current")))?;
        let parts: Vec<_> = target.components().collect();
        if parts.len() != 2
            || parts[0].as_os_str() != "versions"
            || !matches!(parts[1], std::path::Component::Normal(_))
        {
            return Err("No managed OpenAI CUA runtime is selected".into());
        }
        let resources = root.join(target).join(APP).join(RESOURCES);
        let runtime = resources.join("cua_node");
        let plugin = resources.join("plugins/openai-bundled/plugins/chrome");
        let installer = plugin.join("scripts/installManifest.mjs");
        for path in [
            &installer,
            &runtime.join("bin/node"),
            &runtime.join("bin/node_repl"),
            &resources.join("codex"),
        ] {
            if !path.is_file() {
                return Err(format!(
                    "OpenAI browser bridge component is incomplete: {}",
                    path.display()
                ));
            }
        }
        let source = r#"import { pathToFileURL } from 'node:url';
const { install } = await import(pathToFileURL(process.env.NANOCODEX_BROWSER_INSTALLER));
await install({ appServerRuntimePaths: {
  codexCliPath: process.env.NANOCODEX_BROWSER_CODEX,
  nodePath: process.env.NANOCODEX_BROWSER_NODE,
  nodeReplPath: process.env.NANOCODEX_BROWSER_NODE_REPL,
}});"#;
        let output = std::process::Command::new(runtime.join("bin/node"))
            .args(["--input-type=module", "--eval", source])
            .env("NANOCODEX_BROWSER_INSTALLER", &installer)
            .env("NANOCODEX_BROWSER_CODEX", resources.join("codex"))
            .env("NANOCODEX_BROWSER_NODE", runtime.join("bin/node"))
            .env("NANOCODEX_BROWSER_NODE_REPL", runtime.join("bin/node_repl"))
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("Could not start the official browser bridge installer: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "Official browser bridge installer failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(serde_json::json!({"status":"installed","component":"official-browser-bridge"}))
    }

    #[cfg(test)]
    mod tests {
        include!("provision_tests.rs");
    }
}

#[cfg(test)]
mod receipt_tests {
    #[test]
    fn preserves_installed_command_arguments_and_environment() {
        let _compile_windows_installer = super::windows_provision;
        let executable = std::env::current_exe().unwrap();
        let receipt = serde_json::json!({"status":"installed","transport":"mcp","executable":executable,"args":["provider entry.mjs"],"environment":{"CODEX_CLI_PATH":"signed host","BROWSER_USE_TINYSKY_ENABLED":"1"}});
        let config = super::config_from_receipt(&receipt).unwrap();
        assert_eq!(config.executable, executable);
        assert_eq!(config.args, ["provider entry.mjs"]);
        assert_eq!(
            config
                .environment
                .get(std::ffi::OsStr::new("CODEX_CLI_PATH"))
                .unwrap(),
            "signed host"
        );
        assert_eq!(
            config
                .environment
                .get(std::ffi::OsStr::new("BROWSER_USE_TINYSKY_ENABLED"))
                .unwrap(),
            "1"
        );
        assert!(
            super::config_from_receipt(
                &serde_json::json!({"status":"installed","transport":"mcp","executable":"relative"})
            )
            .is_err()
        );
    }
}
