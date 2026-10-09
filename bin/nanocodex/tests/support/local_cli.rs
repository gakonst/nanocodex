//! The local agent command tree is selected by the invoked name `ncl`.
//!
//! Cargo builds one `nanocodex` executable. Local-tree journeys run it through
//! an `ncl` alias next to it, exactly as the installed `bin/ncl` symlink does.

use std::{path::Path, sync::OnceLock};

/// Path to an `ncl` alias of the Cargo-built `nanocodex` executable.
pub(crate) fn local_cli() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let binary = Path::new(env!("CARGO_BIN_EXE_nanocodex"));
        let alias = binary.with_file_name(if cfg!(windows) { "ncl.exe" } else { "ncl" });
        #[cfg(unix)]
        {
            let target = binary.file_name().expect("Cargo binary has a file name");
            match std::os::unix::fs::symlink(target, &alias) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create {}: {error}", alias.display()),
            }
        }
        #[cfg(not(unix))]
        {
            // Copies go stale after a rebuild; refresh on every test process.
            let staged = alias.with_extension(format!("{}.tmp", std::process::id()));
            std::fs::copy(binary, &staged).expect("stage the ncl alias");
            let _ = std::fs::rename(&staged, &alias);
            let _ = std::fs::remove_file(&staged);
        }
        alias.to_str().expect("UTF-8 Cargo target path").to_owned()
    })
}
