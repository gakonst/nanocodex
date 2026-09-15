# Flipper Zero Hand

The native iPhone Hand can bridge a Flipper Zero to a durable nanocodex agent over Bluetooth LE. No custom firmware, public endpoint, Wi-Fi board, or separate daemon is required: the phone speaks Flipper's official protobuf RPC protocol and exposes typed tools through the existing authenticated Hand WebSocket.

## Connect

1. On the Flipper, open **Settings → Bluetooth** and turn Bluetooth on.
2. Keep the Nanocodex iPhone app active for the first connection.
3. Ask the agent to run `flipper_devices`, then `flipper_connect`.
4. Confirm the pairing code on iOS and the Flipper when prompted.

Nanocodex remembers the selected CoreBluetooth device identifier. Later tool calls reconnect the last paired Flipper automatically. A connected BLE session can continue during iOS-granted background execution; iOS still controls suspension and locked-device availability.

The Flipper can have only one companion connection at a time. Disconnect it from the official Flipper mobile app before connecting Nanocodex.

## Tools

| Tool | Effect |
| --- | --- |
| `flipper_devices` | Scan nearby official Flipper BLE service UUIDs |
| `flipper_connect` / `flipper_disconnect` | Manage the encrypted BLE session and remembered device |
| `flipper_info` | Read firmware, hardware, protobuf, battery, and BLE metadata |
| `flipper_list_files` / `flipper_read_file` | Inspect `/ext` and `/int` storage |
| `flipper_write_file` | Create or replace UTF-8 or binary files using chunked RPC writes |
| `flipper_start_app` / `flipper_exit_app` | Control the foreground Flipper application |
| `flipper_press_button` | Send short or long navigation-button input |
| `flipper_screen` | Capture the 128×64 framebuffer as raw bytes and an agent-readable braille preview |

The capability intentionally does not expose raw RPC, factory reset, reboot/DFU, GPIO writes, or unrestricted radio primitives. Higher-level Flipper workflows can be added as typed tools with explicit semantics.

## Protocol boundary

The implementation follows the official [Flipper Zero protobuf definitions](https://github.com/flipperdevices/flipperzero-protobuf) and the BLE serial service used by the official [Flipper iOS app](https://github.com/flipperdevices/Flipper-iOS-App):

- RPC messages are varint-length-delimited protobuf `PB.Main` frames.
- BLE notifications may split or combine frames; the decoder retains incomplete input.
- Writes respect Flipper's four-byte flow-control window and the negotiated GATT write size.
- File writes use 512-byte `Storage.WriteRequest` chunks with one stable command ID and `has_next` framing.
- Only one RPC command is admitted at a time. A timeout or cancellation restarts the BLE RPC session before the next call.
