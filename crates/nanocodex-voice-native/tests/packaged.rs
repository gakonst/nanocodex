//! Exercise the physical helper after moving its entire private native runtime.
use nanocodex_voice_native::{VoiceHost, VoicePackage};
use std::{fs, path::Path, time::Duration};

fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let output = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &output)?;
        } else {
            fs::copy(entry.path(), output)?;
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires pnpm build:voice-native and NANOCODEX_TEST_VOICE_RUNTIME"]
async fn relocated_helper_initializes_negotiates_and_closes() -> anyhow::Result<()> {
    let runtime = std::env::var_os("NANOCODEX_TEST_VOICE_RUNTIME").expect("prepared runtime path");
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("relocated package with spaces");
    copy_tree(Path::new(&runtime), &root.join("nanocodex-resources/voice"))?;
    let package = VoicePackage { package_dir: root };
    let helper = package
        .package_dir
        .join("nanocodex-resources/voice/bin")
        .join(if cfg!(windows) {
            "nanocodex-voice-host.exe"
        } else {
            "nanocodex-voice-host"
        });
    let version = tokio::process::Command::new(helper)
        .arg("--build-commit")
        .output()
        .await?;
    anyhow::ensure!(version.status.success(), "relocated helper loader failed");
    let commit = String::from_utf8(version.stdout)?.trim().to_owned();
    tokio::time::timeout(Duration::from_secs(40), async {
        let host = VoiceHost::connect(&package, &commit)
            .await?
            .initialize_runtime()
            .await?;
        let (host, offer) = host.start_transport().await?;
        anyhow::ensure!(offer.into_sdp().contains("m=audio"), "missing audio offer");
        host.close().await?;
        let host = VoiceHost::connect(&package, &commit)
            .await?
            .initialize_runtime()
            .await?;
        anyhow::ensure!(
            host.initialize_runtime().await.is_err(),
            "duplicate runtime accepted"
        );
        anyhow::ensure!(
            VoiceHost::connect(&package, "wrong-build").await.is_err(),
            "incompatible build accepted"
        );
        anyhow::Ok(())
    })
    .await??;
    Ok(())
}
