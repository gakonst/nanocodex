//! `nanocodex`: the user-facing CLI (managed tree by default; `ncl` or a
//! leading `--local` selects the local agent tree).

fn main() -> std::process::ExitCode {
    nanocodex_cli::cli_main(nanocodex_bin_shared::build_info!())
}
