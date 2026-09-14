# Bundled Node runtime

Nanocodex renders its macOS interface in SwiftUI and AppKit. This directory holds
the private Node runtime for the shared managed-agent/Hand helper. No system Node
installation is required to run a built app.

The Apple Silicon build uses the official Node **24.19.0** archive:

- Source: https://nodejs.org/dist/v24.19.0/node-v24.19.0-darwin-arm64.tar.gz
- SHA-256: `8294b7aa9b03997481c06babf1e8b270c859358f27da57a11509afe537ac381d`
- Published checksums: https://nodejs.org/dist/v24.19.0/SHASUMS256.txt

For a clean development checkout, fetch and verify it explicitly, then extract
`bin/node` here as `node` and its `LICENSE` as `NODE-LICENSE.txt`:

```sh
curl --fail --location https://nodejs.org/dist/v24.19.0/node-v24.19.0-darwin-arm64.tar.gz --output /tmp/nanocodex-node-v24.19.0-darwin-arm64.tar.gz
printf '%s  %s\n' 8294b7aa9b03997481c06babf1e8b270c859358f27da57a11509afe537ac381d /tmp/nanocodex-node-v24.19.0-darwin-arm64.tar.gz | shasum -a 256 --check
mkdir -p macos/Resources/runtime
tar -xOf /tmp/nanocodex-node-v24.19.0-darwin-arm64.tar.gz node-v24.19.0-darwin-arm64/bin/node > macos/Resources/runtime/node
tar -xOf /tmp/nanocodex-node-v24.19.0-darwin-arm64.tar.gz node-v24.19.0-darwin-arm64/LICENSE > macos/Resources/runtime/NODE-LICENSE.txt
chmod 755 macos/Resources/runtime/node
```

The executable and license copy are ignored by git and copied into the app by
Xcode. The official binary links only to macOS system libraries. Generated
shared helper files come from `pnpm --filter @nanocodex/desktop-runtime build`.
