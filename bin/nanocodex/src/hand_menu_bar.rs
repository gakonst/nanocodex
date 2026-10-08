//! A GUI companion for the installed Hand, never another Hand publisher.
use eyre::Result;

/// Companion failures must never undo successful Hand enrollment.
pub(crate) async fn ensure_with_warning(explicit: bool) {
    #[cfg(target_os = "macos")]
    if let Err(error) = ensure(explicit).await {
        eprintln!(
            "Hand menu bar could not be opened: {error:#}. Run `nanocodex hand menu-bar` from this Mac's desktop session to repair it. The Hand service is unchanged."
        );
    }
    #[cfg(not(target_os = "macos"))]
    let _ = explicit;
}

pub(crate) async fn show() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        ensure(true).await?;
        eprintln!("Nanocodex Hand menu bar launch requested. Quit Hand stops the local service.");
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    eyre::bail!("The Hand menu bar is only available on macOS")
}

pub(crate) async fn guide() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::guide().await
    }
    #[cfg(not(target_os = "macos"))]
    eyre::bail!("The Hand permission guide is only available on macOS")
}

#[cfg(target_os = "macos")]
use macos::ensure;

#[cfg(target_os = "macos")]
mod macos {
    use eyre::{Result, WrapErr, bail, eyre};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        process::Output,
        time::Duration,
    };
    use tokio::process::Command;

    const LABEL: &str = "com.nanocodex.hand-menu-bar";
    const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/nanocodex-hand-menu-bar"));
    const INFO: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.nanocodex.hand-menu-bar</string>
<key>CFBundleName</key><string>Nanocodex Hand</string>
<key>CFBundleExecutable</key><string>nanocodex-hand-menu-bar</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSUIElement</key><true/>
<key>LSMinimumSystemVersion</key><string>14.0</string>
</dict></plist>
"#;

    fn regular_or_absent(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_file() => {
                bail!("Refusing to replace nonregular file {}", path.display())
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn directory(path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.is_dir() => {
                bail!("Expected a directory, not a link: {}", path.display())
            }
            Ok(_) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(path)?;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    fn write_changed(path: &Path, bytes: &[u8], executable: bool) -> Result<bool> {
        regular_or_absent(path)?;
        if path.exists() && fs::read(path)? == bytes {
            if executable && fs::metadata(path)?.permissions().mode() & 0o111 == 0 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
                return Ok(true);
            }
            return Ok(false);
        }
        replace_file(path, bytes, executable)?;
        Ok(true)
    }

    fn replace_file(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
        regular_or_absent(path)?;
        let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
        temporary.write_all(bytes)?;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(if executable {
                0o700
            } else {
                0o600
            }))?;
        temporary.as_file().sync_all()?;
        temporary.persist(path)?;
        Ok(())
    }

    async fn run(program: &str, arguments: &[&str]) -> Result<Output> {
        tokio::time::timeout(
            Duration::from_secs(15),
            Command::new(program)
                .args(arguments)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .wrap_err_with(|| format!("{program} timed out"))?
        .wrap_err_with(|| format!("Could not run {program}"))
    }

    async fn launch(arguments: &[&str]) -> Result<()> {
        let result = run("/bin/launchctl", arguments).await?;
        if !result.status.success() {
            bail!(
                "launchctl {} failed: {}",
                arguments[0],
                String::from_utf8_lossy(&result.stderr).trim()
            );
        }
        Ok(())
    }

    async fn unload(service: &str) -> Result<()> {
        launch(&["bootout", service]).await?;
        // bootout can acknowledge before the GUI job disappears. Do not race
        // bootstrap against the previous owner or spawn an unmanaged fallback.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let status = run("/bin/launchctl", &["print", service]).await?;
            if status.status.code() == Some(113) {
                return Ok(());
            }
            if !status.status.success() || tokio::time::Instant::now() >= deadline {
                bail!(
                    "The previous menu bar job has not finished unloading; retry `nanocodex hand menu-bar`"
                );
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    // A GUI child must use the stable launcher across coordinated updates.
    fn cli_path(home: &Path) -> Result<PathBuf> {
        let root = crate::launcher::running_install_root()
            .or_else(|| std::env::var_os("NANOCODEX_DIR").map(PathBuf::from))
            .unwrap_or_else(|| home.join(".nanocodex"));
        let launcher = root.join("bin/nanocodex");
        if !launcher.is_absolute() || !launcher.is_file() {
            bail!(
                "The installed CLI launcher is missing at {}; finish `nanocodex install` first",
                launcher.display()
            );
        }
        if fs::metadata(&launcher)?.permissions().mode() & 0o111 == 0 {
            bail!(
                "The installed CLI launcher is not executable: {}",
                launcher.display()
            );
        }
        Ok(launcher)
    }

    pub(super) async fn guide() -> Result<()> {
        ensure(true).await?;
        let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre!("HOME is unset"))?);
        let bundle = home.join(".nanocodex/menu-bar/Nanocodex Hand.app");
        let cli = cli_path(&home)?;
        let output = Command::new("/usr/bin/open")
            .arg("-n")
            .arg(bundle)
            .arg("--args")
            .arg("--cli")
            .arg(cli)
            .arg("--permission-guide")
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "Could not open the Hand permission guide: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    pub(super) async fn ensure(explicit: bool) -> Result<()> {
        let home = PathBuf::from(std::env::var_os("HOME").ok_or_else(|| eyre!("HOME is unset"))?);
        if !home.is_absolute() {
            bail!("HOME must be absolute");
        }
        let uid = run("/usr/bin/id", &["-u"]).await?;
        if !uid.status.success() {
            bail!("Could not identify the desktop user");
        }
        let uid = String::from_utf8(uid.stdout)?.trim().parse::<u32>()?;
        if uid == 0 {
            bail!("Run as the desktop user, without sudo");
        }
        let domain = format!("gui/{uid}");
        if !run("/bin/launchctl", &["print", &domain])
            .await?
            .status
            .success()
        {
            bail!("No logged-in macOS desktop session is available");
        }
        let cli = cli_path(&home)?;
        let root = home.join(".nanocodex");
        directory(&root)?;
        let directory_path = root.join("menu-bar");
        directory(&directory_path)?;
        let lock_path = directory_path.join("install.lock");
        regular_or_absent(&lock_path)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock)
            .wrap_err("Another menu bar installation is in progress")?;
        let bundle = directory_path.join("Nanocodex Hand.app");
        let contents = bundle.join("Contents");
        let executable_directory = contents.join("MacOS");
        let helper = executable_directory.join("nanocodex-hand-menu-bar");
        let agents = home.join("Library/LaunchAgents");
        directory(&home.join("Library"))?;
        directory(&agents)?;
        let plist = agents.join(format!("{LABEL}.plist"));
        regular_or_absent(&plist)?;
        let first_install = !plist.exists();
        let helper_text = helper
            .to_str()
            .ok_or_else(|| eyre!("Non-UTF-8 home path"))?;
        let previous_definition = if !first_install {
            let old = run(
                "/usr/bin/plutil",
                &["-convert", "json", "-o", "-", plist.to_str().unwrap()],
            )
            .await?;
            if !old.status.success() {
                bail!("Could not read the existing menu bar LaunchAgent");
            }
            let old: Value = serde_json::from_slice(&old.stdout)?;
            if old["Label"] != LABEL || old["ProgramArguments"][0] != helper_text {
                bail!("The menu bar LaunchAgent has an unexpected owner; it was left unchanged");
            }
            Some(old)
        } else {
            None
        };
        let service = format!("{domain}/{LABEL}");
        let status = run("/bin/launchctl", &["print", &service]).await?;
        let loaded = status.status.success();
        if !loaded && status.status.code() != Some(113) {
            bail!(
                "Could not inspect the menu bar LaunchAgent: {}",
                String::from_utf8_lossy(&status.stderr).trim()
            );
        }
        if loaded && first_install {
            bail!("A menu bar job is loaded without its installed plist; it was left unchanged");
        }
        let running = loaded
            && String::from_utf8_lossy(&status.stdout)
                .lines()
                .any(|line| line.trim().starts_with("pid = "));
        for path in [&bundle, &contents, &executable_directory] {
            directory(path)?;
        }
        // Signing an app binds its executable to Info.plist and resources and
        // changes the Mach-O signature. Remember both source and installed hashes
        // so a correctly signed, unchanged helper is not replaced on every run.
        let receipt_path = directory_path.join("bundle-receipt.json");
        regular_or_absent(&receipt_path)?;
        regular_or_absent(&helper)?;
        let source_hash = hex::encode(Sha256::digest(HELPER));
        let installed_hash = if helper.exists() {
            Some(hex::encode(Sha256::digest(fs::read(&helper)?)))
        } else {
            None
        };
        let receipt: Option<Value> = if receipt_path.exists() {
            serde_json::from_slice(&fs::read(&receipt_path)?).ok()
        } else {
            None
        };
        let helper_changed = receipt.as_ref().is_none_or(|receipt| {
            receipt["source_sha256"].as_str() != Some(source_hash.as_str())
                || receipt["installed_sha256"].as_str() != installed_hash.as_deref()
        });
        if helper_changed {
            write_changed(&helper, HELPER, true)?;
        }
        let info_changed = write_changed(&contents.join("Info.plist"), INFO, false)?;
        let signature = contents.join("_CodeSignature");
        directory(&signature)?;
        regular_or_absent(&signature.join("CodeResources"))?;
        let valid_signature = run(
            "/usr/bin/codesign",
            &["--verify", "--strict", bundle.to_str().unwrap()],
        )
        .await?
        .status
        .success();
        let bundle_changed = helper_changed || info_changed || !valid_signature;
        if bundle_changed {
            // Replace the inode before re-signing so a running helper keeps its
            // previous intact executable until the companion-only reload.
            replace_file(&helper, HELPER, true)?;
            let signed = run(
                "/usr/bin/codesign",
                &["--force", "--sign", "-", bundle.to_str().unwrap()],
            )
            .await?;
            if !signed.status.success() {
                bail!(
                    "Could not sign the Hand menu bar app: {}",
                    String::from_utf8_lossy(&signed.stderr).trim()
                );
            }
            let verified = run(
                "/usr/bin/codesign",
                &["--verify", "--strict", bundle.to_str().unwrap()],
            )
            .await?;
            if !verified.status.success() {
                bail!("The Hand menu bar app signature did not verify");
            }
            let receipt = json!({"source_sha256": source_hash, "installed_sha256": hex::encode(Sha256::digest(fs::read(&helper)?))});
            write_changed(&receipt_path, &serde_json::to_vec(&receipt)?, false)?;
        }
        let log = directory_path.join("menu-bar.log");
        regular_or_absent(&log)?;
        let _log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log)?;
        let definition = json!({
            "Label": LABEL,
            "ProgramArguments": [helper_text, "--cli", cli],
            "RunAtLoad": true,
            "KeepAlive": {"SuccessfulExit": false},
            "LimitLoadToSessionType": "Aqua",
            "ProcessType": "Interactive",
            // A menu-triggered service handover must outlive Quit or a helper update.
            "AbandonProcessGroup": true,
            "ThrottleInterval": 10,
            "WorkingDirectory": home,
            "EnvironmentVariables": {"HOME": home, "PATH": "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"},
            "StandardOutPath": log,
            "StandardErrorPath": log
        });
        // Compare parsed values: plutil's XML dictionary ordering is not an
        // ownership or update signal and must not restart an unchanged helper.
        let plist_changed = previous_definition.as_ref() != Some(&definition);
        if plist_changed {
            let mut temporary = tempfile::NamedTempFile::new_in(&agents)?;
            temporary.write_all(&serde_json::to_vec(&definition)?)?;
            let converted = run(
                "/usr/bin/plutil",
                &[
                    "-convert",
                    "xml1",
                    "-o",
                    "-",
                    temporary.path().to_str().unwrap(),
                ],
            )
            .await?;
            if !converted.status.success() {
                bail!("Could not encode the menu bar LaunchAgent");
            }
            write_changed(&plist, &converted.stdout, false)?;
        }
        let changed = bundle_changed || plist_changed;
        if explicit {
            // An explicit repair/show also reverses a disabled companion job.
            // Automatic enrollment preserves the user's launchd opt-out.
            launch(&["enable", &service]).await?;
        }
        // A successful user Quit leaves a loaded, stopped job. Preserve it on
        // automatic repair, even when new bytes were staged for the next launch.
        if loaded && running && changed {
            unload(&service).await?;
            launch(&["bootstrap", &domain, plist.to_str().unwrap()]).await?;
        } else if !loaded && (first_install || explicit) {
            launch(&["bootstrap", &domain, plist.to_str().unwrap()]).await?;
        } else if loaded && !running && explicit {
            // Automatic repair may have updated the plist while respecting a
            // previous Quit. launchd still caches the old arguments: reload it.
            unload(&service).await?;
            launch(&["bootstrap", &domain, plist.to_str().unwrap()]).await?;
        }
        Ok(())
    }
}
