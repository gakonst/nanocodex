# Nanocodex development

- `js/nanocodex` and `js/nanocodex-react` are stable contracts. Cover changes
  with focused contract, type, package, and runtime tests.
- `js/nanocodex-vite` owns the Vite plugin, WASM build, OAuth relay, and Cloudflare Vite
  integration. Do not recreate those responsibilities in apps or adapters.
- Adapters are not a dumping ground. Put behavior at its real owner and expose a
  narrow entrypoint.
- Apps and Workers are independent deployables. Never import another app's or
  Worker's source; shared code needs an explicit package owner.
- During JS/platform work, do not edit Rust unless the user explicitly requests
  it. Generated WASM is the Rust boundary.
- Product code gets almost no unit tests: keep only pure policy and important
  protocol boundaries. Require canonical browser and real service/Worker
  evidence for product behavior.
- `pnpm dev` uses Turbo to start the complete stack through Portless at
  `https://nanocodex.localhost`; linked worktrees get
  `https://<branch>.nanocodex.localhost`. The paired playground is
  `https://playground.nanocodex.localhost` or
  `https://<branch>.playground.nanocodex.localhost`. With a high
  `PORTLESS_PORT`, append that port to each URL. Deploy from the root, in order,
  with `pnpm deploy:egress`, `pnpm deploy:managed`,
  `pnpm deploy:connect-dialog`, `pnpm deploy:connect-api`,
  `pnpm deploy:chief-of-staff`, `pnpm deploy:connect-playground`, and
  `pnpm deploy:account`. Each component script must build its complete workspace
  dependency graph from a clean checkout; do not manually prebuild omitted
  packages.
- pnpm owns the JavaScript workspace, Turbo owns task concurrency and build
  caching, Portless owns local names and ports, and Vite/Wrangler own runtime.
- Do not add custom stack, deploy, test, rollout, probe, or verification wrappers.
  Keep installs, migrations, resource changes, deployment, and evidence explicit.
- CI deployment starts independently of tests. Pull requests dry-run stateful
  Workers and publish Cloudflare previews only for Workers that support them;
  `master` deploys the production configs directly.
- Wrangler upload success is not behavior evidence. Exercise the exact changed
  journey, inspect console/network/storage/sockets/CSP, and verify provider
  secrets never reach browser or app surfaces.
- The product journeys are: passkey account reauthentication; two durable agent
  turns across reload/reconnect; exact Connect approval projection; connector or
  MCP connect/call/revoke fencing; attachment lifecycle; and cross-account
  isolation. Test only the journeys touched by a change.
- Preserve Cloudflare names, bindings, migrations, grants, durable state, public
  exports, and customer behavior during moves.
- Never mix, delete, or reset concurrent work; never commit secrets, generated
  builds, caches, retained jobs, or another user's files.
