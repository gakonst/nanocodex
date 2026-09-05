# Automatic iPhone updates

The `iOS TestFlight` workflow builds and uploads the native Inbox app on every
push to `master` and every published release. Manual runs upload only when
targeting `master`; other refs and pull requests build an unsigned Release
archive without accessing signing secrets. Uploads are serialized and never
cancelled in progress. Builds are marked **TestFlight Internal Only**; this does
not submit an App Store release or distribute to external testers.

The Apple app currently lives in PR #267. Merge that app and this workflow into
`master` before expecting master pushes to deliver it. Release tags must include
the `apple/` target. The existing `Apple Inbox` workflow exercises the native UI;
this workflow separately checks the actual device Release archive and resources.

## One-time Apple setup

Use your paid Apple Developer team and the existing bundle identifier
`xyz.paradigm.nanocodex.inbox` to retain the app's identity. If your installed
development copy uses a different identifier, use that same identifier in both
the Xcode project and the workflow before provisioning.

1. In [Certificates, Identifiers & Profiles](https://developer.apple.com/account/resources/),
   register that explicit app identifier if it is not already registered.
2. Create an iOS app record in [App Store Connect](https://appstoreconnect.apple.com/apps)
   for that identifier. Choose a unique display name if Nanocodex Inbox is taken;
   keep the bundle identifier unchanged.
3. Export an **Apple Distribution** certificate **with its private key** from
   Keychain Access as a password-protected `.p12`. Create an **App Store Connect**
   distribution provisioning profile for the identifier and that certificate.
   Development, ad hoc, and enterprise profiles are rejected by the workflow.
4. Create a team API key in App Store Connect → Users and Access → Integrations.
   An App Manager key can upload builds. Save its Key ID, Issuer ID, and downloaded
   `.p8` private key. Do not paste private keys into a PR or commit them.
5. Add these repository [Actions secrets](https://github.com/gakonst/nanocodex/settings/secrets/actions):

   | Secret | Value |
   | --- | --- |
   | `IOS_DISTRIBUTION_P12_BASE64` | Base64 of the exported distribution `.p12` |
   | `IOS_DISTRIBUTION_P12_PASSWORD` | Password used when exporting the `.p12` |
   | `IOS_PROVISIONING_PROFILE_BASE64` | Base64 of the App Store distribution `.mobileprovision` |
   | `APP_STORE_CONNECT_KEY_ID` | API key ID |
   | `APP_STORE_CONNECT_ISSUER_ID` | API issuer ID |
   | `APP_STORE_CONNECT_PRIVATE_KEY` | Complete contents of the `.p8` file, with real newlines |

   The team ID and profile UUID are read from the profile, not configured twice.
   To upload local files directly from a Mac with authenticated GitHub CLI:

   ```sh
   base64 -i distribution.p12 | gh secret set IOS_DISTRIBUTION_P12_BASE64 --repo gakonst/nanocodex
   base64 -i inbox.mobileprovision | gh secret set IOS_PROVISIONING_PROFILE_BASE64 --repo gakonst/nanocodex
   gh secret set IOS_DISTRIBUTION_P12_PASSWORD --repo gakonst/nanocodex
   gh secret set APP_STORE_CONNECT_KEY_ID --repo gakonst/nanocodex
   gh secret set APP_STORE_CONNECT_ISSUER_ID --repo gakonst/nanocodex
   gh secret set APP_STORE_CONNECT_PRIVATE_KEY --repo gakonst/nanocodex < AuthKey.p8
   ```

6. In the app's TestFlight tab, create an **internal** tester group, enable
   **automatic distribution**, and add your App Store Connect user to the group.
   Accept the invitation yourself. No external beta review is needed for internal
   testing. A public TestFlight invitation link is not an internal tester group.
7. Run `iOS TestFlight` against `master` and wait for the upload and Apple's build
   processing. Install from TestFlight once, then turn on **Automatic Updates**
   on the app's TestFlight page. Later builds update the existing installation;
   there is no need to delete it.

## Operation

- Open Inbox settings → Open TestFlight to manually install an available update.
  This opens the TestFlight app; it does not claim an update is available or force
  iOS to install one. Automatic installation timing is controlled by iOS.
- CI supplies a run/attempt build number and lets Xcode resolve upload collisions.
  The app's marketing version remains owned by the Xcode project. Re-running an
  upload can produce another build; it does not replace an existing Apple build.
- Credential values are only available to upload steps. Temporary credentials and
  the signing keychain are removed even on failure. No signing assets are uploaded
  as Actions artifacts. Missing credentials fail with their secret names.
- Renew the certificate/profile before they expire. TestFlight builds expire
  after 90 days; a newer build starts its own testing period.
- The app uses platform HTTPS/Keychain encryption, declared as exempt, and declares
  app-local UserDefaults access in its privacy manifest. Reassess these declarations
  if the app adds custom cryptography or changes its API/data usage.

References: [Apple internal testers](https://developer.apple.com/help/app-store-connect/test-a-beta-version/add-internal-testers/),
[TestFlight automatic updates](https://testflight.apple.com/),
[GitHub signing on macOS runners](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications).
