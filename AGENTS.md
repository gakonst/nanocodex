# Nanocodex development

- Never add unit tests after implementing the code. Define expected behavior
  and failure cases before implementation.
- Strongly prefer end-to-end tests as the only testing layer needed for a
  feature. Exercise complex features through the relevant runtime and finish
  each E2E run with an inspectable, reproducible artifact (for example a trace,
  transcript, or screenshot plus the command, inputs, and expected outcome).
- When isolation is necessary, enumerate the system's failure modes first,
  before writing implementation or test code. Keep isolated tests only when
  they catch concrete bugs the E2E coverage misses, such as protocol violations,
  authorization failures, races, or recovery errors. Remove tests that merely
  mirror implementation details, incidental source spelling, or mock setup.
- Prefer a small set of behavioral scenarios over parallel suites for every
  helper or wrapper. When a journey or protocol test covers the same failure,
  remove the redundant lower-level cases, unused fixtures, test-only APIs, and
  obsolete runner references.
- Do not test source text, private layouts, method presence, fixed prompt/UI
  copy, or a mock's own behavior. A security or regression label does not make
  these checks evidence of runtime behavior. Use compiler, lint, and package
  checks for static contracts; test authorization and recovery by exercising
  the boundary and observing the result.

- `macos/` owns the desktop app and native tiled workspace; `js/desktop-runtime`
  owns its runtime. `apple/NanocodexInbox` targets iPhone and iPad.
- `js/nanocodex` and `js/nanocodex-react` are public contracts. Cover changes
  with relevant contract, type, package, and runtime checks.
- `js/nanocodex-vite` owns the Vite plugin, WASM build, OAuth relay, and
  Cloudflare Vite integration.
- Apps and Workers deploy independently. Shared behavior belongs in a package,
  consumed through its public API.
- Use root `pnpm` scripts and existing Turbo/Portless/Vite/Wrangler tooling.
  Deploy dependencies first and `account` last. Component deploy scripts build
  their dependencies from a clean checkout. See [README.md](README.md) for setup.
- Use synthetic identities and project data in fixtures and examples. Keep real
  account IDs, private project inventories, and one-off personal migration plans
  outside tracked source; pass operational data through private runtime inputs.
