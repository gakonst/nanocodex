# Phone-driven Nanocodex delivery

Nanocodex owns the build request and update UI. A Cloudflare Sandbox Hand
dispatches a clean GitHub-hosted macOS builder; Apple performs distribution and
iOS performs installation. No nearby Mac, USB cable, Tailscale network, or
third-party sideloading app is part of the path.

## Request a Nanocodex self-update

From a checked-out repository in a Cloudflare Sandbox Hand:

```sh
bash apple/scripts/request-self-update.sh
```

The helper starts `ios-self-update.yml`, waits for the exact correlated run, and
writes `/brain/ios-deployments/latest.json`. The workflow archives the current
`xyz.paradigm.centaur` app with Xcode, signs it on the ephemeral runner, uploads
it as an internal-only TestFlight build, and deletes the temporary signing
keychain. The phone can then use Settings → Nanocodex updates. Apple controls
processing and final installation timing.

The Cloudflare sandbox image already includes `gh`; Nanocodex provides its
scoped GitHub authentication through egress. Never put GitHub or Apple private
keys in `/brain` or a sandbox workspace.

## One-time Apple setup

Use the existing **Centaur by Paradigm** App Store Connect record for bundle ID
`xyz.paradigm.centaur`. Create an Apple Distribution certificate, a matching
App Store provisioning profile, an App Store Connect API key, and an internal
TestFlight group with automatic distribution. Configure these Actions secrets:

| Secret | Value |
| --- | --- |
| `IOS_DISTRIBUTION_P12_BASE64` | Password-protected distribution certificate and private key |
| `IOS_DISTRIBUTION_P12_PASSWORD` | Password for that p12 |
| `IOS_PROVISIONING_PROFILE_BASE64` | App Store profile for `xyz.paradigm.centaur` |
| `IOS_SHARE_PROVISIONING_PROFILE_BASE64` | App Store profile for `xyz.paradigm.centaur.share` |
| `IOS_WIDGETS_PROVISIONING_PROFILE_BASE64` | App Store profile for `xyz.paradigm.centaur.widgets` |
| `APP_STORE_CONNECT_KEY_ID` | API key ID |
| `APP_STORE_CONNECT_ISSUER_ID` | API issuer ID |
| `APP_STORE_CONNECT_PRIVATE_KEY` | Complete p8 contents |

The workflow validates the profile type, bundle ID, and expiry before importing
the certificate. Pull requests only create an unsigned device archive and
cannot read or upload with these secrets.

## New apps built by Nanocodex

`latest.json` is intentionally a deployment-provider receipt rather than a raw
IPA handoff. The first provider is `apple` / `testflight-internal`. New bundle
IDs can use the same build request contract once their App Store records and
profiles exist. For instant private fleet installation, add an Apple device
management provider; enrolled devices can receive managed custom or proprietary
apps wirelessly. A normal iOS app cannot install an arbitrary unsigned IPA or
act as its own package manager.

The first implementation therefore proves the supported self-update path while
leaving the provider boundary ready for MDM. It does not depend on Linux SDK
extraction projects: those can compile selected SwiftPM apps, but this app uses
an Xcode project and Apple's distribution services.
