# Development

Run these commands from a repository checkout. The root web stack uses Node.js
24, pnpm 11.25.0, Rust 1.97, and wasm-bindgen-cli 0.2.126. The Rust compiler needs
the `wasm32-unknown-unknown` target.

```sh
rustup toolchain install 1.97
rustup override set 1.97
rustup target add wasm32-unknown-unknown
cargo install --locked wasm-bindgen-cli --version 0.2.126
corepack enable
pnpm install --frozen-lockfile
pnpm build
pnpm dev
```

The local Rust override selects the compiler for this checkout.
`pnpm dev` starts the account web app and Connect playground through Portless.
Local HTTPS needs one-time certificate trust. Binding port 443 may need
administrator approval on macOS; `PORTLESS_PORT=1355 pnpm dev` uses an
unprivileged port and adds `:1355` to the local URLs.

Portless's proxy is shared by the user. Application routes, processes, and
Wrangler state are isolated per checkout and worktree.

## Checks

Follow [AGENTS.md](../AGENTS.md) for testing and shared-checkout conventions.
For Rust changes, `pnpm check:fast` runs formatting and the CI Clippy command on
crates changed since `origin/master` and their workspace dependents. Use the
affected package's documented checks and user journeys for other changes.

## Native apps and bindings

- [Mac app](../macos/README.md): Xcode setup and the bundled Node runtime.
- [iPhone and iPad app](../apple/README.md): project, signing, and device setup.
- [Python](../py/bindings/README.md): Maturin and the native PyO3 binding.
- [JavaScript packages](../js/README.md): WASM bindings, UI packages, and Workers.
- [Examples](../examples/README.md): commands for individual SDK consumers.

## Deployments

Apps and Workers deploy independently. Deploy dependencies before their
consumers, and deploy the account app last. Each deployment script builds its
dependencies from a clean checkout. Read the service guides for configuration
and credentials:

- [Managed agent Worker](../js/managed/README.md)
- [Account app](../js/account/README.md)
- [Credential broker](../js/egress/README.md)
- [Public X browsing](../js/x-api/README.md)
