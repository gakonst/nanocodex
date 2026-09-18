# WoW accessibility snapshot producer

This optional example connects the original NanocodexObserve addon to the generic Hand external-snapshot provider. No game names or addon APIs are present in the provider core. The addon works alone; BlindSlash supplies additional narration when installed. Third-party addon code is not included.

1. Copy `NanocodexObserve/` into WoW's `Interface/AddOns/` and reload the UI. Check the TOC against the client version. Optional BlindSlash setup also uses KeyboardPort and TomTom.
2. In the game run `/ncobserve`, then Ctrl+A, Ctrl+C. Escape closes the export panel. This is an explicit interaction, not part of passive `observe()`.
3. In the same user's Linux desktop session run:

```sh
python3 publish.py --clipboard --app 'WoW' --window 'World of Warcraft' \
  --output "$HOME/.local/state/nanocodex/observations/game.json"
```

Or use `--input export.json` instead of `--clipboard` for an explicitly exported file. The producer refuses unrelated clipboard contents, oversized input, and captures older than 60 seconds. It retains the original capture timestamp and atomically writes a mode-0600 file. It prioritizes labels, omits edit-box text, and reports truncation. The Hand and producer must run under the same OS user for direct file ingestion.

4. Start the native Hand with `NANOCODEX_OBSERVATION_SNAPSHOT_PATHS` containing the **absolute** output path as a JSON array. For example:

```sh
export NANOCODEX_OBSERVATION_SNAPSHOT_PATHS="[\"$HOME/.local/state/nanocodex/observations/game.json\"]"
```

5. Use `computer({action:"observe", context:{app:"WoW",window:"World of Warcraft"}})`. The selector must exactly match the snapshot identity. This is explicitly selected app context, not verification of the foreground window. Read `result.observation` alongside the screenshot. After five seconds the provider marks this snapshot stale; publish again only through an explicit capture operation.

The 8-KiB transport projection is intentionally smaller than the full addon export. It cannot describe the complete 3D world and cannot expose restricted game values. SavedVariables snapshots update on reload/logout, not continuously. This example does not provide a background input agent, continuous clipboard watcher, or 10-Hz controller.

Tests:

```sh
python3 -m unittest discover -s . -p 'test_*.py'
(cd NanocodexObserve && lua test_mock.lua > /dev/null)
```

The original addon was tested live on Omarchy with Retail 12.1.0.69814: its exports included quest labels/status and BlindSlash narration. The generic-provider integration additionally replays that captured data while preserving its timestamp. Current desktop-session AT-SPI validation is reported separately from the addon proof.


## Classic Beta live validation (2026-09-18)

The same read-only addon was installed into `_classic_beta_/Interface/AddOns/`
and loaded with `/reload` on Classic Beta 1.60.1.69893 (interface 16001). A new
Orc Warrior entered Valley of Trials; bounded autorun, stop, turning and jumping
were observed through the live screen. The explicit `/ncobserve` copy/publish
flow returned 36 UI elements through `computer.observe`, including the current
character name and zone, with `freshness: fresh` and age 4,409 ms. The export
remained `partial: true`; it is not a complete 3D-world or combat observation.

Publish to the exact path configured in the desktop publisher's
`NANOCODEX_OBSERVATION_SNAPSHOT_PATHS`. Writing another similarly named snapshot
file does not update that provider: it can continue returning an older capture
with the same app/window identity. Check capture time and fresh character/zone
labels alongside the screenshot before acting. This is still an explicit export,
not continuous telemetry or a high-frequency control loop.
