# Experimental crates

Only computer and evaluation packages remain experimental:

- [`nanocodex-computer`](nanocodex-computer/README.md): persistent computer-use
  tools and the separately built native runtime. The library is published as an
  explicitly experimental dependency of the supported VM integration.
- [`nanocodex-eval`](nanocodex-eval/README.md): VM-backed benchmark scheduling,
  verification, durable evidence, and differential analysis.
- `nanocodex-eval-adapters`: application-specific evaluation adapters.

Evaluation crates remain unpublished. Experimental describes API stability;
these packages still pass the applicable formatting, Clippy, documentation,
test, cancellation, tracing, and benchmark checks.

Browser, egress, Hand, VM, and voice packages now live directly under `crates/`.
The VM package's explicit dependency on the published experimental computer
library is supported; it does not promote the computer API to stable status.
