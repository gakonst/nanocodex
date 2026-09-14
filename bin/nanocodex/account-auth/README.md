# CLI account authentication

This unpublished package owns SMS sign-in, stdin API-key import, local account
credential storage, and environment selection for both Nanocodex CLIs. The
managed transport remains independent of environment and filesystem policy.

`Account` supplies the shared clap command group. `AccountCommand`, `Login`,
and `Options` also support direct command aliases. `client_from_environment`
selects the exact managed origin and its environment or saved API key.
`managed_url_from_environment` selects only an origin, allowing system VM hosts
to keep their separate token-based authorization without reading account keys.

See the [user guide](../nanocodex2/README.md#account-sign-in) for command usage.
The account protocol follows `js/desktop-runtime/src/auth.mjs` and
`js/managed/src/account-auth.ts`. Local Axum fixtures exercise both executable
entrypoints without sending SMS or modifying real accounts:

```sh
cargo test -p nanocodex-cli-auth
cargo test -p nanocodex-bin -p nanocodex2-bin --test account_auth
```
