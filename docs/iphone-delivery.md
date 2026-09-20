# iPhone delivery from a Mac

The local OTA path builds the `xyz.paradigm.centaur` app with the Mac's Xcode
signing credentials and serves it at
https://nanocodex-ios-updates.gakonst.workers.dev. The iPhone can download over
cellular or Wi-Fi; the Mac and phone do not need to share a network. iOS performs
the installation after the user confirms its prompt.

## Build and publish

Use a Mac with Xcode, Python 3.9 or later, the repository's pnpm dependencies,
Cloudflare deployment access, and working automatic signing for the app, share
extension, and widget extension. The target device must be registered in each
development provisioning profile. A development-signed installation also needs
Developer Mode enabled on the phone. Keep signing credentials in the Mac's
normal keychain and Xcode account settings.

Choose a monotonically increasing positive integer build number. Use one
persistent asset directory outside every repository checkout; keep this entire
directory between publications and deployments. It contains the full release
history. Do not use a temporary directory or commit exported IPAs.

```sh
# Replace these example values with your next build and chosen persistent path.
bash apple/scripts/build-mac-update.sh 123 "$HOME/Library/Application Support/Nanocodex/ota-assets" \
  --deploy --notes 'Improved iPhone updates'
```

The script rebuilds the shared voice core, performs a Release archive, exports
with Xcode's `debugging` method and automatic signing, validates the IPA, and
publishes it. The printed archive/export directory is retained for diagnosis. Xcode may need
to refresh profiles through its configured account. Pass `--device-udid` or set
`NANOCODEX_DEVICE_UDID` privately to require that every profile includes a
specific phone. No device identifier is stored in this repository or feed.

To validate and prepare an existing exported IPA without deploying:

```sh
python3 apple/scripts/publish-mac-update.py --ipa /path/to/Nanocodex.ipa \
  --assets-dir "$HOME/Library/Application Support/Nanocodex/ota-assets"
```

Add `--deploy` to invoke `pnpm --filter nanocodex-managed-service exec wrangler deploy --config
apple/ota/wrangler.jsonc --assets ASSETSDIR`. `--config` can specify another
Wrangler configuration for the same publication origin. Preparation changes the
local feed only; deployment must succeed before the phone can see it. On a
failed deployment rerun with the same IPA and asset directory. Do not run
independent publishers with different asset directories: the local lock and
latest-build policy protect one persistent directory, not competing hosts.

Validation checks the expected bundle ID, a numeric build, strict code signatures,
and unexpired profiles for every app and extension. App Store-only profiles are
rejected. Specifying the device additionally checks installation eligibility in
every profile. An existing build cannot be replaced with different IPA bytes or
manifest metadata, and `latest.json` cannot move to a lower build. Keep a backup
of the asset directory; deploying an incomplete directory removes older assets
from the deployed site. If the directory is lost, restore it before publishing.

## Install and update

For the first installation, open the site above in **Safari on the registered
iPhone**, tap **Install Nanocodex**, and accept the iOS installation prompt.
After bootstrapping, open **Settings → Nanocodex updates** in Nanocodex to check
for and install a newer build. iOS controls the final installation; opening the
installation link does not itself prove completion. Expired profiles need a new
signed build. Cellular downloading still depends on the phone's connectivity
and data settings.

The static feed has this schema (`notes` is optional):

```json
{
  "version": "1.0",
  "build": "123",
  "bundle_id": "xyz.paradigm.centaur",
  "manifest_url": "https://nanocodex-ios-updates.gakonst.workers.dev/builds/123/manifest.plist",
  "published_at": "2026-09-20T12:00:00Z",
  "notes": "Improved iPhone updates"
}
```

Each immutable `/builds/<build>/` contains `Nanocodex.ipa`, `manifest.plist`, an
installation page, and `sha256.txt`. The root page and `latest.json` use
`Cache-Control: no-store`; build assets use immutable caching. `_headers` also
sets the IPA and plist content types and disallows indexing. Anyone with an
asset URL can download it; signing and device provisioning control whether iOS
can install it. Check the hosting plan's per-asset size limit before deployment.

Run the focused policy tests with:

```sh
python3 apple/scripts/test-publish-mac-update.py
```

## Optional TestFlight delivery

`bash apple/scripts/request-self-update.sh` remains available from a Cloudflare
Sandbox Hand. It dispatches `ios-self-update.yml`, waits for the correlated
GitHub-hosted macOS build, and writes a delivery receipt under
`/brain/ios-deployments`. The workflow uses App Store Connect and Apple-managed
TestFlight distribution, which has separate processing and installation timing.
Its receipt is separate from this static OTA `latest.json`.

The TestFlight workflow requires the existing App Store Connect record and its
Actions signing secrets: `IOS_DISTRIBUTION_P12_BASE64`,
`IOS_DISTRIBUTION_P12_PASSWORD`, `IOS_PROVISIONING_PROFILE_BASE64`,
`IOS_SHARE_PROVISIONING_PROFILE_BASE64`, `IOS_WIDGETS_PROVISIONING_PROFILE_BASE64`,
`APP_STORE_CONNECT_KEY_ID`, `APP_STORE_CONNECT_ISSUER_ID`, and
`APP_STORE_CONNECT_PRIVATE_KEY`. Configure an internal TestFlight group with
automatic distribution. Keep private keys out of `/brain` and workspaces.
