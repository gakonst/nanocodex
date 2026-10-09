// SPDX-License-Identifier: Apache-2.0

//! Shared TUI render and history benchmarks. The cases live in the library
//! (`src/nanocodex2/tui/bench.rs`, feature `tui-bench`) so they exercise the
//! production renderer rather than a copy of it.

criterion::criterion_main!(nanocodex_cli::tui_benches);
