# WoW phone controller mapping

The phone exposes the physical Xbox-style gamepad understood by the host: two analog sticks, stick clicks, four face buttons, D-pad, shoulders, triggers, View and Menu. It preserves the standardized gamepad wire names. It does not translate buttons into keyboard shortcuts or infer gameplay actions from held modifiers.

## Provenance

Verified 2026-09-19 against Blizzard's exported UI source, distributed by the Gethe mirror, `live` revision `78282522143e25c3540583734fd192c3d69be910`:

[SharedConstants.lua, lines 61–107](https://github.com/Gethe/wow-ui-source/blob/78282522143e25c3540583734fd192c3d69be910/Interface/AddOns/Blizzard_SharedXML/SharedConstants.lua#L61-L107) defines the gamepad key glyphs. This is Blizzard source mirrored by a third party, not a Blizzard-hosted repository.

| Phone / wire control | WoW physical key |
| --- | --- |
| A / a | PAD1 |
| B / b | PAD2 |
| X / x | PAD3 |
| Y / y | PAD4 |
| D-pad up/down/left/right | PADDUP / PADDDOWN / PADDLEFT / PADDRIGHT |
| LB / leftShoulder | PADLSHOULDER |
| RB / rightShoulder | PADRSHOULDER |
| LT / leftTrigger | PADLTRIGGER |
| RT / rightTrigger | PADRTRIGGER |
| L3 / leftStick | PADLSTICK |
| R3 / rightStick | PADRSTICK |
| View / back | PADBACK |
| Menu / start | PADFORWARD |

The source confirms button identity and glyphs. It does not prove the current character's action bindings, modifier settings, target behavior, action pages, focus, addons, or device remaps. The phone receives no live binding snapshot. Therefore labels stay physical under every modifier combination and refer the player to WoW's current prompts. The guide pauses input; closing it leaves input paused until Resume.

## Verification and limits

`swift test --package-path apple/NanocodexRemote --filter RemoteNativeGameInputStateTests`: 10 tests pass. Coverage includes the 16 physical controls and all 16 trigger/shoulder combinations, preservation of simultaneous inputs, neutral reset, analog bounds, and non-overlapping landscape control frames including the minimum supported 536 × 218 content area.

The real running client's settings have not been read or modified in this change. A bounded read-only search on the game host found no accessible `Config.wtf`; this is not proof that no settings exist. Current custom bindings remain unknown. No phone install, live game input, service restart, or in-game gameplay verification was performed. Before claiming end-to-end correctness, check the connected client's controller configuration and observe each physical control, simultaneous modifiers, and neutral release in WoW.

`xcodebuild -project apple/NanocodexInbox.xcodeproj -scheme NanocodexInbox -destination 'generic/platform=iOS Simulator' -derivedDataPath .build/wow-controller-evidence/DerivedData CODE_SIGNING_ALLOWED=NO build`: BUILD SUCCEEDED, including the iOS-only SwiftUI controller.

The gameplay toolbar also exposes microphone and speaker buttons in both native gamepad and keyboard/mouse modes. Microphone enable requires an explicit tap with control and the advertised microphone capability; tapping while pending or enabled requests disable. Speakers toggle independently. This follow-up uses the integrating RemoteViewer audio API (`supportsMicrophone`, `microphoneEnabled`, `microphonePending`, `microphoneError`, `setMicrophoneEnabled`, `speakersEnabled`, `setSpeakersEnabled`). The follow-up passes Swift iOS syntax parsing; the successful app build above predates it. Type-check and exercise the buttons after integrating that API and RemotePeer audio implementation.

The inherited bounded simulator UI attempt selected `RemoteScreenLifecycleUITests/testSyntheticNativeGamepadTouchStatesStopExitAndBackground`. It remained in simulator test-runner launch for more than 15 minutes without a completed result bundle; the specific `xcodebuild` process was stopped. `GamepadUI.xcresult` had no `Info.plist`, and the launch diagnostic reported `NSMachErrorDomain -308` (server died). This attempt is inconclusive and supplies no UI interaction pass. Re-run the focused fixture test after the simulator runner is healthy and the audio bases are integrated; keep that evidence separate from the successful physical-controller build and unit suite.
