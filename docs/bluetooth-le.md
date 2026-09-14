# Bluetooth LE Hand

The native iPhone Hand exposes a protocol-neutral Bluetooth Low Energy GATT bridge to a nanocodex agent. It can scan peripherals, connect, inspect discovered services and characteristics, read and write values, subscribe to notifications, and collect notification frames during a finite window.

This is the common transport layer for working with devices whose application protocol is known or can be described to the agent. The existing Flipper Zero tools remain a typed profile because they add protobuf framing, flow control, and higher-level operations on top of BLE.

## Tools

| Tool | Operation |
|---|---|
| `bluetooth_devices` | Scan all BLE peripherals or filter by advertised service UUID/name prefix |
| `bluetooth_connect` | Connect by returned device ID and discover all or selected GATT services |
| `bluetooth_services` | Inspect characteristic UUIDs and read/write/notify properties |
| `bluetooth_read` | Read a characteristic as base64, hex, and UTF-8 when valid |
| `bluetooth_write` | Write an explicit UTF-8, hex, or base64 value |
| `bluetooth_subscribe` | Enable or disable notifications/indications |
| `bluetooth_collect` | Collect subscribed notification frames for a finite interval |
| `bluetooth_disconnect` | End the generic BLE connection |

For a new device, scan with its advertised service UUID when known, connect using the returned `device_id`, and inspect `bluetooth_services` before reading, writing, or subscribing. Writes are checked against the characteristic properties and the negotiated single-write size.

## Scope

The bridge works with BLE GATT peripherals available to CoreBluetooth. It does not expose Bluetooth Classic, OS-owned profiles such as audio or keyboards, or a proprietary application protocol by itself. Pairing and permission prompts remain owned by iOS. Background notification delivery is best-effort under iOS `bluetooth-central` rules; a continuously running agent call is not guaranteed while the app is suspended.

Raw GATT access intentionally does not guess commands. Device-specific protocol semantics should be provided in the request or added later as typed tools when repeated workflows warrant it.
