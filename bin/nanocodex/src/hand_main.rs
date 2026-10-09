//! `nanocodex-hand`: the Hand daemon and its daemon-side entrypoints. Installed
//! under the historical file name `nanocodex2`.

fn main() -> std::process::ExitCode {
    nanocodex_cli::hand_main()
}
