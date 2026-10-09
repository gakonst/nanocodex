//! The local agent command tree is selected by the invoked name `ncl`.
//!
//! Cargo builds the `nanocodex` CLI. Local-tree journeys run it as `ncl`, the
//! name the installed `bin/ncl` alias provides. The alias is a per-process
//! hard link (or copy), so journeys that canonicalize the path still see `ncl`
//! and a rebuilt CLI is never confused with a stale alias.

use std::{path::Path, sync::OnceLock};

/// Path to an `ncl` alias of the Cargo-built `nanocodex` executable.
pub(crate) fn local_cli() -> &'static str {
    static PATH: OnceLock<String> = OnceLock::new();
    PATH.get_or_init(|| {
        let binary = Path::new(env!("CARGO_BIN_EXE_nanocodex"));
        let directory = binary
            .with_file_name("local-cli")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create the ncl alias directory");
        let alias = directory.join(if cfg!(windows) { "ncl.exe" } else { "ncl" });
        if std::fs::hard_link(binary, &alias).is_err() {
            std::fs::copy(binary, &alias).expect("stage the ncl alias");
        }
        // The CLI forwards daemon work to the Hand beside it.
        let hand = binary.with_file_name(if cfg!(windows) {
            "nanocodex-hand.exe"
        } else {
            "nanocodex-hand"
        });
        if hand.is_file() {
            let _ = std::fs::hard_link(&hand, directory.join(hand.file_name().unwrap()));
        }
        alias.to_str().expect("UTF-8 Cargo target path").to_owned()
    })
}
