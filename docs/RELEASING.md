# Releasing Nanocodex

Nanocodex follows the shared-version workspace pattern used by Alloy. The
publishable crates are released together in dependency order by
[`cargo-release`](https://github.com/crate-ci/cargo-release); application and
binding crates are explicitly excluded from crates.io publication.

## Prepare

1. Start from a clean, up-to-date `master` checkout.
2. Move the relevant entries in `CHANGELOG.md` out of `Unreleased` and verify
   every CI job is green.
3. Install `cargo-release` and authenticate Cargo with a crates.io token.
4. Preview the release without changing the repository:

   ```sh
   cargo release VERSION
   ```

## Publish

Run the signed shared-version release and push its commit and tag:

```sh
cargo release VERSION --execute
git push origin master --follow-tags
```

`cargo-release` publishes the crates in dependency order and creates a single
`VERSION` tag. The tag starts `.github/workflows/release.yml`, which builds the
native `nanocodex` CLI for Linux, macOS, and Windows and attaches the artifacts
to a generated GitHub release. Crates.io credentials remain local to the
release operator; GitHub Actions only receives the repository-scoped token it
needs to publish release artifacts.
