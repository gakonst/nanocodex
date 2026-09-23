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
mod mac {
    use sha2::{Digest, Sha256};
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    // Official URLs and signing identity from OpenAI codex revision 36430b3688,
    // codex-rs/cli/src/desktop_app/mac.rs. No third-party package or URL override.
    const ARM_DMG: &str = "https://persistent.oaistatic.com/codex-app-prod/Codex.dmg";
    const X64_DMG: &str = "https://persistent.oaistatic.com/codex-app-prod/Codex-latest-x64.dmg";
    const TEAM: &str = "2DC432GLL2";
    const BUNDLE: &str = "com.openai.codex";
    const REQUIREMENT: &str = "=identifier \"com.openai.codex\" and anchor apple generic and certificate leaf[subject.OU] = \"2DC432GLL2\"";
    const APP: &str = "Codex.app";
    const RESOURCES: &str = "Contents/Resources";
    const MODULES: &str = "cua_node/lib/node_modules";
    const SKY: &str = "@oai/sky/Codex Computer Use.app";
    const ENTRY: &str = "@oai/cua-repl/bin/cua-repl.mjs";
    // Keep aligned with KNOWN_GUI_BUILD in the embedded readiness module.
    // A contract test catches drift before either component ships.
    const SUPPORTED_GUI_BUILD: &str = "9922";

    pub(super) trait Commands {
        fn run(&mut self, program: &str, args: &[OsString]) -> Result<String, String>;
        fn check_cancelled(&self) -> Result<(), String> {
            Ok(())
        }
    }

    const CANCELLED: &str = "OpenAI CUA setup cancelled";

    pub(super) struct Cancellation(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Cancellation {
        pub(super) fn new() -> Self {
            Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )))
        }
        pub(super) fn flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
            self.0.clone()
        }
    }
    impl Drop for Cancellation {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    pub(super) struct System {
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl System {
        pub(super) fn new(cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
            Self { cancelled }
        }
    }

    // The group leader's PID stays reserved until its final group signal. Never
    // probe/reap it in Drop before terminating descendants which may own pipes.
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
                // process_group(0) gives this command its own group; an unreaped
                // leader's PID cannot be recycled into an unrelated process.
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
            let cleanup =
                program == "/usr/bin/hdiutil" && args.first().is_some_and(|arg| arg == "detach");
            // Detach still runs after cancellation, with its own deadline.
            if !cleanup {
                self.check_cancelled()?;
            }
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
            let started = std::time::Instant::now();
            let mut cancelled = false;
            let status = loop {
                if (!cleanup && self.cancelled.load(Ordering::Acquire))
                    || (cleanup && started.elapsed() > std::time::Duration::from_secs(10))
                {
                    child.kill();
                    cancelled = true;
                    break child.wait().map_err(|error| error.to_string())?;
                }
                // Do not reap while descendants still own pipes: keeping the PID
                // reserved makes group cancellation safe if they block forever.
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
                return Err(if cleanup {
                    "OpenAI CUA cleanup timed out"
                } else {
                    CANCELLED
                }
                .into());
            }
            if !status.success() {
                return Err(format!(
                    "{program} failed ({status}): {}",
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

    fn verify(app: &Path, commands: &mut impl Commands) -> Result<String, String> {
        commands.run(
            "/usr/bin/codesign",
            &args(
                &[
                    "--verify",
                    "--deep",
                    "--strict",
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
        if build.is_empty()
            || build.len() > 80
            || !build
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
        {
            return Err("OpenAI bundle has an invalid build identifier".into());
        }
        if build != SUPPORTED_GUI_BUILD {
            return Err(format!(
                "Unsupported OpenAI CUA build {build}; the managed host supports build {SUPPORTED_GUI_BUILD}. Update Nanocodex when support for this build is available"
            ));
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
        ] {
            let path = resources.join(relative);
            if !path.is_file() {
                return Err(format!(
                    "OpenAI CUA runtime is incomplete: {}",
                    path.display()
                ));
            }
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

    // Cache the result of deep signature verification, never a guessed version
    // or a top-level mtime. Every nested entry participates in the fingerprint.
    // A changed, expired, unsupported or unreadable record goes through the
    // original full signature/build/runtime checks. Cache failures are harmless.
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
            && record.build == SUPPORTED_GUI_BUILD
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
            if error == CANCELLED || error.starts_with("Unsupported OpenAI CUA build ") {
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
        (
            "openai-cua-gui-readiness.mjs",
            include_str!("openai-cua-gui-readiness.mjs"),
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
        // A cache hit must not republish an identical receipt on every startup.
        // Keep existing running clients' metadata and file watchers quiet.
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

    fn download(stage: &mut Staging, commands: &mut impl Commands) -> Result<PathBuf, String> {
        // sysctl detects Apple Silicon even when Nanocodex runs under Rosetta.
        let arm = cfg!(target_arch = "aarch64")
            || commands
                .run(
                    "/usr/sbin/sysctl",
                    &["-n".into(), "hw.optional.arm64".into()],
                )
                .is_ok_and(|v| v.trim() == "1");
        let url = if arm { ARM_DMG } else { X64_DMG };
        let dmg = stage.path.join("upstream.dmg");
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
                "--output".into(),
                dmg.into_os_string(),
                url.into(),
            ],
        )?;
        let mount = stage.path.join("mount");
        io(fs::create_dir(&mount))?;
        // A panic or interrupted worker must never recursively remove a mount.
        stage.cleanup = false;
        let attached = commands.run(
            "/usr/bin/hdiutil",
            &[
                "attach".into(),
                "-readonly".into(),
                "-nobrowse".into(),
                "-noautoopen".into(),
                "-mountpoint".into(),
                mount.as_os_str().to_owned(),
                stage.path.join("upstream.dmg").into_os_string(),
            ],
        );
        let result = attached.and_then(|_| {
            let source = ["ChatGPT.app", "Codex.app"]
                .into_iter()
                .map(|name| mount.join(name))
                .find(|path| path.is_dir())
                .ok_or("Official OpenAI disk image contains no supported app bundle")?;
            verify(&source, commands)?;
            let destination = stage.path.join("payload").join(APP);
            commands.run(
                "/usr/bin/ditto",
                &[source.into_os_string(), destination.as_os_str().to_owned()],
            )?;
            Ok(destination)
        });
        // Also attempt detachment after a failed attach: hdiutil can fail after
        // mounting. Never recursively clean a directory that is still mounted.
        let detached = commands.run("/usr/bin/hdiutil", &args(&["detach"], &mount));
        if let Err(error) = detached {
            stage.cleanup = false;
            return Err(format!(
                "Could not detach installer at {}: {error}; staging retained at {}",
                mount.display(),
                stage.path.display()
            ));
        }
        stage.cleanup = true;
        result
    }

    pub(super) fn provision(
        root: &Path,
        applications: &[PathBuf],
        refresh: bool,
        commands: &mut impl Commands,
    ) -> Result<serde_json::Value, String> {
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
        let installed = if refresh {
            None
        } else {
            applications
                .iter()
                .flat_map(|dir| ["ChatGPT.app", "Codex.app"].map(|name| dir.join(name)))
                .find(|app| app.is_dir() && verify(app, commands).is_ok())
        };
        let app = if let Some(source) = installed {
            let destination = stage.path.join("payload").join(APP);
            commands.run(
                "/usr/bin/ditto",
                &[source.into_os_string(), destination.as_os_str().to_owned()],
            )?;
            destination
        } else {
            download(&mut stage, commands)?
        };
        // Copying must preserve every signed resource; verify the destination.
        let build = verify(&app, commands)?;
        if refresh
            && let Ok(Some(existing)) = cached(root, commands, true)
            && existing["build"].as_str() == Some(&build)
        {
            return Ok(existing);
        }
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
