# Structured context alongside screen observations

A screen observation combines pixels with optional, attributed structured context. The screen remains useful when a provider is missing, slow, stale, or unsupported. Structured context is application data, not an instruction source or proof of permission to act.

Native Hands own collection. The managed service transports bounded provider results into both the model-visible text and the structured computer-tool result. Ordinary video frames do not traverse accessibility trees. Screenshot-only clients remain compatible.

## Providers

Linux accessibility uses AT-SPI2 in the desktop user's session. Availability of the registry does not imply that an application exposes useful elements. Missing bindings, session access, or an accessible application should be reported rather than inferred as an empty but complete tree.

External snapshot producers publish a locally configured JSON snapshot. The reader does not run commands from that JSON and does not poll the clipboard. Producers own the application-specific acquisition step. An external provider requires an explicit app/window selector and exact identity match before releasing data. This is requested context, not proof that the application is foreground: results carry `scope: requested_context` and `foreground_verified: false`. Consumers must reconcile that context with the screenshot. Without a selector, AT-SPI can select a window marked active by the accessibility implementation.

Both kinds of provider report provenance, collection time, freshness and bounded/partial coverage. The screenshot and semantic context are collected separately; timestamps allow consumers to assess skew. This is not an atomic compositor-and-application transaction. UI coordinates must retain their coordinate system and scale rather than being assumed to match resized screenshot pixels.

## Application adapters

`examples/observation-providers/wow` contains the original addon from the live proof, plus a producer that wraps its explicit copy-panel export in the generic external snapshot format. Third-party BlindSlash, KeyboardPort and TomTom code is not vendored. WoW is not referenced by core observation collection.

The addon adds structured UI labels and bounded narration history. It cannot provide a complete 3D scene or unrestricted combat state. It omits restricted values and edit-box text. Its copy panel is an explicit interaction; passive observation reads the resulting snapshot and reports its age. SavedVariables are written at reload/logout, not a continuous transport.

## Interpretation and control

Vision consumes the screenshot and can reconcile it with provider text. No separate vision-model dependency is introduced by the provider contract. Providers neither choose actions nor grant permission. An observation failure must not trigger an automatic retry of a click, key, or other potentially non-idempotent input.

## Local configuration and wire format

On the native Hand, `NANOCODEX_OBSERVATION_SNAPSHOT_PATHS` is a JSON array of up to four absolute snapshot paths. The agent cannot add paths through tool arguments. Files must be regular, owned by the Hand user, and not symlinks. Do not point a Hand at another desktop session's data.

`NANOCODEX_OBSERVATION_ATSPI_BUS` explicitly selects the desktop session D-Bus address. The private Linux capture desktop must not accidentally borrow the publisher's ambient bus. Python 3 and PyGObject/AT-SPI bindings are required for this provider. VM screen publishers do not read the parent host's registry or files; guest-provider forwarding is not implemented in this change.

An external producer atomically writes:

```json
{"schemaVersion":1,"capturedAt":1789630874000,"app":"Example","window":"Example window","data":{"labels":["Save"],"partial":false}}
```

`capturedAt` is Unix milliseconds from acquisition, not file publication. Data is bounded to 8 KiB with separate structural and string limits. Provider results distinguish errors/unavailability from successful-but-partial data and mark snapshots older than five seconds as stale. A stale snapshot can remain useful context; it is not a current-frame assertion.

```javascript
const result = await tools.computer({
  action: "observe",
  context: { app: "Example", window: "Example window" }
});
image(result);
text(result.observation);
```

The context selector is available only on `observe`, not input actions. Optional provider failures leave the screenshot available. The initial implementation uses bounded subprocess collection rather than a persistent accessibility cache and is not a 10-Hz control loop.

## Publisher integration

The Go `hands/remote` publisher uses the same provider contract and embedded
Python collector on the agent completion path. Viewer video and frame captures
do not collect semantic data. `nanocodex-remote observe-local` exercises the
read-only screenshot/provider path without a broker using local configuration.

Top-level `observation.capturedAt` anchors the request; each provider's
`capturedAt` describes acquisition. Neither is an atomic screenshot timestamp.
AT-SPI bounds are labeled `coordinateSpace: atspi_reported_screen` and
`boundsVerified: false`; reconcile them with the screenshot before input.
