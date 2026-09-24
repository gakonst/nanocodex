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

## Audio and verification

The gameplay toolbar exposes microphone and speaker buttons in both native
gamepad and keyboard/mouse modes. Microphone enable requires an explicit tap with
control and the advertised microphone capability; tapping while pending or enabled
requests disable. Speakers toggle independently when connected to a Hand that
advertises speaker support. The controls use the integrated
[RemoteViewer audio API](../apple/NanocodexRemote/Sources/NanocodexRemote/RemoteSession.swift).

For end-to-end verification, check the connected client's controller configuration
and observe each physical control, simultaneous modifiers, and neutral release in
WoW. A simulator or physical-button state test cannot establish the game's current
custom bindings.
