# JavaScript-to-TypeScript migration

The first slice moves the Connect protocol, Connect UI policy, Connect API policy,
playground grant presentation, and shared remote-MCP target validation to checked
TypeScript. Published `.mjs` compatibility is retained with `.mts` sources and
compiler-generated `.mjs` plus `.d.mts` output. Tests and scripts remain JavaScript
unless TypeScript materially strengthens a shipped contract.

## Remaining inventory

After this slice, the `js` workspace contains 144 shipped implementation `.mjs`
files, 73 JavaScript test/support files, and 17 JavaScript scripts. The shipped
implementations are:

- `nanocodex`: 110 (`actions` 5, `browser` 16, `cloud` 17, `cloudflare` 5,
  `host` 2, package roots 2, `managed` 5, `node` 6, `runtime` 19, `tools` 31,
  `worker` 2).
- `nanocodex-tools`: 18.
- `nanocodex-vite`: 10.
- `nanocodex-react`: 5.
- `account/container`: 1.

There are 79 remaining declarations paired with JavaScript implementations:
`nanocodex` 62, `nanocodex-tools` 9, `nanocodex-vite` 4,
`nanocodex-react` 3, and one account script declaration. Four declaration-only
contracts (`nanocodex/types.d.mts`, `nanocodex/wasm.d.mts`,
`nanocodex/cloud/types.d.mts`, and `nanocodex-tools/tools/types.d.mts`) have no
JavaScript implementation counterpart and should remain type-only unless their
owner changes.

## Safe sequence

1. Convert `nanocodex-tools` runtime/tool entrypoints into its existing TypeScript
   build, keeping every exported subpath and adding package/type/runtime evidence.
2. Convert `nanocodex-vite` to `.mts` emission without moving its Vite, WASM,
   OAuth relay, or Cloudflare responsibilities.
3. Establish generated runtime/declaration output for `nanocodex`, then migrate
   leaf layers before entrypoints: actions and pure runtime policy; shared tools;
   node and browser; cloud, managed, and Cloudflare; package roots and Workers.
4. Convert `nanocodex-react` after it consumes generated `nanocodex` declarations,
   preserving the root, `connect`, and `agent` contracts with package/type/runtime
   tests.
5. Convert the account container relay when its Node execution path can consume
   emitted output. Revisit scripts and tests only where type checking adds value.
