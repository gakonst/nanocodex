# Native voice libraries in Codex releases

The macOS Codex release package includes dynamically linked GStreamer and GLib
libraries and selected plugins, plus their native library dependencies. These
components have their own copyrights and licenses. Their notices accompany
this file in `licenses/`: `LGPL-2.1.txt` for GStreamer and GLib,
`proxy-libintl.txt` for libintl, `libffi.txt`, `PCRE2.md` and `sljit.txt`
for PCRE2, `Opus.txt`, and `zlib.txt`. GVDB is included with GLib under LGPL.

The exact upstream versions, source archive URLs and SHA-256 digests are in
`sources.json` alongside this notice. The corresponding build, runtime
projection and package scripts are in the public Codex source tree under
`third_party/voice/`. The source commit for this package is recorded in
`manifest.json`. The native libraries remain separate dynamic libraries in
`lib/` and `plugins/`; replacing them requires compatible binaries and valid
macOS code signatures. Build tools listed in `sources.json` are build inputs,
not bundled runtime libraries.
