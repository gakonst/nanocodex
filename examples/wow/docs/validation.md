# Validation and deployment

The initial Omarchy deployment runs as the gaming desktop user through a user systemd service. The addon is installed in the existing Retail client's Interface/AddOns directory. Local source lives at `the source checkout`.

Verified during development:

- Python backend regression suite: same-origin/Host enforcement, size limits, private account-store validation, immutable existing-agent settings, source-validated new Luna request schema, event normalization, idempotency and local transcription requests.
- Lua addon stub suites: missing/modern/Classic API paths, restricted values, context JSON, lifecycle, slash commands and copy UI.
- Chromium browser regression: project and thread selection, work/game conversation isolation, nested addon context imports, deferred response polling and text-only rendering.
- Real local whisper.cpp transcription of a known speech fixture as WAV and Opus WebM.
- Full Chromium MediaRecorder → local HTTP endpoint → whisper.cpp → editable draft, using a simulated microphone carrying the speech fixture. This is not a physical microphone test.

Live account sign-in was absent for both the shell and gaming desktop user. No successful authenticated project request or live model response is claimed. The running client is Classic Beta 1.60.1 build 69913, interface 16001, in Valley of Trials. Installed into `_classic_beta_` and observed `/nc show` opening the real native panel. Live testing caught the Bindings.xml TOC loader warning; optional XML entry bindings were removed, retaining `/nc` and desktop shortcuts. Authenticated model replies remain unverified until a local account key is imported. Browser/system speech voices and the user's physical microphone still require live validation.

Project/chat update:

- 56 Python tests passed, including local organization ownership, persistence, credential isolation, idempotent creation reconciliation, strict metadata permissions, turn control and pagination.
- Browser fixtures pass project/chat creation, local rename/close/restore, earlier pages, draft isolation, targeted stop/steer, imported-action review, foreign ID rejection, clipboard export and narrow viewport checks. Fixtures are not live account proof.
- All Lua suites pass native snapshot browsing/selection, malformed-input bounds, action export, settings and context regressions. Game questions remain available separately through `/nc game` or Game / Copy.
- Updated backend and UI were installed on Omarchy; the user service reports active and its real capabilities endpoint responds. `/nc projects` opens in the real Classic Beta client and accepts clipboard text after a focus fix.
- The live import attempt exposed tab-to-space clipboard normalization. The source parser now accepts encoded fields separated by tabs or spaces and has a regression check. Screen control became unavailable before reloading this final parser fix; successful live workspace import/selection is not yet claimed.
- Actual project/thread snapshot used for live UI testing came from this authorized Nanocodex project's task registry, not the unauthenticated local backend.
- Local account status still reports connected:false. No live authenticated create/send/rename/close/stop operation is claimed. Account API key entry, final addon reload, and live account workflow remain outstanding.
