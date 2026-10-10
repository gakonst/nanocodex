//! `nanocodex-hand`: the Hand daemon and its daemon-side entrypoints. Installed
//! under the historical file name `nanocodex2`.

fn main() -> std::process::ExitCode {
    nanocodex_hand_daemon::hand_main(nanocodex_bin_shared::build_info!())
}
