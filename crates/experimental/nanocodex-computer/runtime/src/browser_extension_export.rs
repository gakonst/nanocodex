//! Ship the independently implemented extension with the companion binary.
use crate::Result;
use std::{fs, io::Write, path::Path};

const FILES: &[(&str, &[u8])] = &[
    (
        "manifest.json",
        include_bytes!("../extensions/chrome/manifest.json"),
    ),
    (
        "background.js",
        include_bytes!("../extensions/chrome/background.js"),
    ),
    (
        "leases.js",
        include_bytes!("../extensions/chrome/leases.js"),
    ),
    (
        "js-kernel.js",
        include_bytes!("../extensions/chrome/js-kernel.js"),
    ),
    (
        "js-kernel.html",
        include_bytes!("../extensions/chrome/js-kernel.html"),
    ),
    (
        "js-kernel-protocol.js",
        include_bytes!("../extensions/chrome/js-kernel-protocol.js"),
    ),
    (
        "js-kernel-worker.js",
        include_bytes!("../extensions/chrome/js-kernel-worker.js"),
    ),
    (
        "js-kernel-offscreen.js",
        include_bytes!("../extensions/chrome/js-kernel-offscreen.js"),
    ),
    (
        "js-kernel-offscreen.html",
        include_bytes!("../extensions/chrome/js-kernel-offscreen.html"),
    ),
];

/// Export to a new directory. Installing into a browser profile is a separate,
/// explicit host action; neither MCP initialization nor a model cell does it.
pub fn export_extension(destination: &Path) -> Result<serde_json::Value> {
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(destination)?;
    let result = (|| -> Result<()> {
        for (name, bytes) in FILES {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(destination.join(name))?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        for (name, _) in FILES {
            let _ = fs::remove_file(destination.join(name));
        }
        let _ = fs::remove_dir(destination);
    }
    result?;
    Ok(
        serde_json::json!({"directory":destination.canonicalize()?,"nativeHost":"org.nanocodex.computer","files":FILES.iter().map(|(name,_)|name).collect::<Vec<_>>()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_binary_exports_complete_extension_and_preserves_existing_files() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("extension");
        let result = export_extension(&destination).unwrap();
        assert_eq!(result["files"].as_array().unwrap().len(), FILES.len());
        for (name, bytes) in FILES {
            assert_eq!(fs::read(destination.join(name)).unwrap(), *bytes);
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(destination.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["name"], "Nanocodex Computer Use");
        assert!(
            fs::read_to_string(destination.join("background.js"))
                .unwrap()
                .contains("org.nanocodex.computer")
        );
        fs::write(
            destination.join("manifest.json"),
            "existing user configuration",
        )
        .unwrap();
        assert!(export_extension(&destination).is_err());
        assert_eq!(
            fs::read_to_string(destination.join("manifest.json")).unwrap(),
            "existing user configuration"
        );
    }
}
