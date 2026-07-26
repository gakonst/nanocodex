# Code Mode locale / ICU gap

Nanocodex Code Mode runs on embedded QuickJS. Stock Codex Code Mode runs on a
V8 build that includes ICU. That difference is intentional and currently
unshimmed: Nanocodex does not pretend to provide locale-aware `Date` / `Intl`
parity.

## Measured behavior

Against the embedded QuickJS host used by `nanocodex-tools` (verified
2026-07-26):

| Expression | Embedded QuickJS result |
| --- | --- |
| `typeof Intl` | `"undefined"` |
| `new Date(Date.UTC(2020, 0, 15)).toLocaleDateString("de-DE")` | `"01/15/2020"` (not German locale formatting) |
| `new Intl.NumberFormat("de-DE").format(1234.5)` | throws `ReferenceError: Intl is not defined` |

An ICU-capable Codex V8 build formats the same date and number with German
locale conventions. Exact parity therefore requires an ICU-capable runtime, not
a partial JavaScript shim.

## Guidance for contributors

- Do not add ad-hoc locale polyfills that only cover a subset of `Intl`.
- Treat locale-sensitive Code Mode scripts as out of scope for request-parity
  claims unless the differential explicitly records this deliberate difference.
- Keep the regression locked by
  `embedded_quickjs_lacks_icu_locale_apis` in
  `crates/nanocodex-tools/src/code_mode/tests.rs`.
- If a future product slice needs ICU, evaluate replacing or rebuilding the
  embedded runtime rather than patching individual formatters.

## Related

- [`PLAN.md`](../PLAN.md) Codex parity checkpoint
- [`benchmarks/codex_request_parity.py`](../benchmarks/codex_request_parity.py)
