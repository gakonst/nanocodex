# Source provenance

This standalone repository was created on 2026-09-19 from the independent, unversioned `nanocodex-wow` project. `SOURCE-MANIFEST.json` records hashes and byte sizes of the selected original source files before changes. No commit ancestry is claimed.

The snapshot allowlists source code, tests, documentation and package metadata. It excludes credentials/account configuration, virtual environments, dependencies, caches, runtime state and logs, research captures, release archives, the machine-specific update launcher, and compiled helpers. Copied symlinks are forbidden. Historical machine paths and desktop usernames in documentation were generalized; the optional voice runtime defaults to the current user's data directory. The production service URL is an application endpoint, not an account identifier.

No live installation, game input, publisher restart, WoW restart or account access was performed while preparing this integration source. The tests use mocks, temporary state and local fixtures.

The resumed integration also retains the original 40 static companion UI assets plus their manifest, verified against the original manifest SHA-256 and byte sizes. Their upstream source and private-use provenance remain in `docs/visual-assets.md`; no new redistribution license is claimed. These assets are required by the existing CSS and were omitted from the initial code-only import.

The monorepo import is based on isolated source commit `4c5234a` (implementation `23b77a2`). The exported archive SHA-256 was `6b9f26b8e718a6b60f37a5bee17c354a622acce7b9947b384885bfd0eb52a1c8`; the temporary worker checkpoint was excluded from this import. Integration here does not install or activate the addon.

Import normalization: removed trailing blank lines in three source/document files; behavior unchanged.
