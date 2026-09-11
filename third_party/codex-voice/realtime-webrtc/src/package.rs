//! Nanocodex package discovery. Native libraries are never loaded in this process.
use std::path::PathBuf;

pub const BUILD_COMMIT: &str = match option_env!("STABLE_GIT_COMMIT") {
    Some(commit) => commit,
    None => "dev",
};

#[derive(Clone)]
pub struct VoicePackage {
    pub package_dir: PathBuf,
}

pub(crate) fn discover() -> Option<VoicePackage> {
    if let Some(root) = std::env::var_os("NANOCODEX_VOICE_PACKAGE") {
        return Some(VoicePackage { package_dir: PathBuf::from(root).canonicalize().ok()? });
    }
    let executable = std::env::current_exe().ok()?.canonicalize().ok()?;
    executable.ancestors().skip(1).take(3).find_map(|root| {
        root.join("nanocodex-resources/voice").is_dir().then(|| VoicePackage {
            package_dir: root.to_path_buf(),
        })
    })
}
