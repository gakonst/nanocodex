# Pinned GBK/GB18030 mapping

`gb18030-2022.ucm` and `LICENSE` are unchanged official ICU 78.3 files from
commit `21d1eb0f306e1141c10931e914dfc038c06121da`. The complete mapping replaces
the version-dependent data for these two TextDecoder labels; it is not a list
of substitutions for observed examples. Node 24.19.0 deliberately maps the
GBK label to its GB18030 ICU converter.

The package was built by Debian ICU 72.1 tools, allowing the same data to load
on the validated Linux ICU 72.1 and macOS ICU 78.3 runtimes. Its SHA256 is
`c889173af9119a778669432f36f431a76d3c64818b435e90d8d1d3e7704bf4da`.
Converter format is 6.2; package format is 1.0; byte order is little endian.
The source version, compiler version, and loaded ICU version are separate
fields in the runtime's `decoder_info.privateMappingStatus` diagnostics.

Reproduce the package with ICU 72.1 `makeconv` and `pkgdata`:

```sh
makeconv -d OUTPUT_DIRECTORY gb18030-2022.ucm
pkgdata -p skyre_icu783_gb18030 -m common -s OUTPUT_DIRECTORY -d OUTPUT_DIRECTORY package-list.txt
```

`package-list.txt` contains one line: `gb18030-2022.cnv`. The
[sealed replay](../../evidence/runtime-icu-data/20260907T000225696697Z/final-linux/commands.json)
records the complete commands and compiler identity; its full source and
license bytes and regenerated package hashes are retained alongside the
original-kernel oracle. No converter compiler or Node process runs in production.

Rust embeds the package with 16-byte alignment. The selected ICU library and
immutable primary stay pinned for process lifetime. Each decoder gets a
thread-safe ICU clone and owns its mutable state. Host reset/drop closes its
clones; it cannot unload the library behind another Host's function pointers.

The initial ICU data directory must be empty for private initialization:
otherwise ICU's normal individual-file search can override registered data.
In that case the implementation preserves the configured system-converter
behavior and reports the reason rather than modifying global ICU settings.
Earlier APIs, big-endian packages, registration conflicts, and Windows
system-ICU package behavior do not inherit the successful Linux/macOS parity
claim. Existing portable fallback remains available with explicit diagnostics.

The [official source](https://github.com/unicode-org/icu/blob/21d1eb0f306e1141c10931e914dfc038c06121da/icu4c/source/data/mappings/gb18030-2022.ucm)
is redistributed with the complete [ICU license](LICENSE).
