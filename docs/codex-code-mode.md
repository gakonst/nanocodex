# Code Mode execution parity

This change builds on the shared tool-contract PR and keeps QuickJS as the
production evaluator. It aligns the tested native and JavaScript behavior for
value serialization, session storage, wait budgets, cancellation/completion
races, yield grace, and multimodal output accounting.

`scripts/codex-parity/native-behavior.py` consumes an external checkout of Codex
commit `36430b36881cf5c289cb48e671cfc9e8b542ae7b` to regenerate bounded fixtures.
The upstream V8 implementation is a test oracle, not a production dependency.
JavaScript tests exercise the helper corpus through the native JS host, QuickJS,
and worker evaluator. No upstream source tree is vendored.

Known limits: immediate notification injection into an active model turn is not
implemented; JavaScript audio accounting covers PCM WAV rather than every
format decoded by native Rust. QuickJS resource limits and engine-specific
behavior remain different. These tests establish the listed cases, not complete
Codex runtime equivalence.
