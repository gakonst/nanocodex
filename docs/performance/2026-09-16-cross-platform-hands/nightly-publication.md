# Nightly delivery — September 16, 2026

Published [immutable nightly 92528e17](https://github.com/gakonst/nanocodex/releases/tag/nightly-92528e17b4feb63fed238abc2f2b766777fc24ff)
and the rolling `nightly` pointer. [Artifact and installation evidence](nightly-publication-measurements.json)
contains all uploaded SHA256 digests, source identities and verification results.

All three build jobs in [run 35154748579](https://github.com/gakonst/nanocodex/actions/runs/35154748579)
passed: Mac, Linux and static Linux VM guest. The publication job remained queued.
After downloading and checking those completed CI artifacts, we cancelled only
that queued run and executed its original source revision's publication script
locally. The workflow's overall cancellation therefore does not indicate a build
failure. Publication preserved its immutable-release and rolling-checksum-marker
ordering; the rolling marker was uploaded after all data assets.

Both downloaded checksum manifests were compared with the CI bytes and GitHub's
uploaded asset digests. The immutable release has ten assets; rolling has
seventeen, including the compatibility raw binaries. Mac helper strict signature
and hypervisor entitlement checks passed.

`nanocodex update --nightly` installed and activated the complete Mac CLI bundle:
`nanocodex`, `nanocodex2`, computer companion, and voice resources. All three
installed executable hashes match the released artifacts; all 37 voice resource
files and the voice archive receipt match. A second ordinary update returned
“already active.” This uses the normal launcher and updater.

The actual published `nanocodex2` then completed the same installed-app journey:
connect the shared native Hand and VM factory, execute on the Mac, create a Linux
VM on that Mac and execute inside it. Both OS markers and the test marker were
verified, the app's Hand survived CLI exit, and the scratch agent was deleted.
This validates the shipped CLI against the installed app and configured GPU host.

Other changes landed on master during release compilation. These results apply
to the explicitly pinned nightly source `92528e17`, installed app source
`8b4077a4` and GPU helper source `e33c804a`, rather than claiming validation of
subsequent unrelated commits. All requested Hand/VM executable fixes in this
report are included in the nightly. Worker and native Swift changes were deployed
and installed separately; see the component and Apple app reports.
