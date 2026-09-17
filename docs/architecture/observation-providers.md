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

## Design reference: the TypeSafe Doom demonstration

The September 15, 2026 [demo](https://x.com/CompleteSkeptic/status/2099925687465570372) shows separate fire, goal and movement decisions and a control graph. TypeSafe's [technical explanation](https://typesafe.ai/blog/introducing-system-one-models-and-jev) explicitly says the model consumes structured state rather than images and reports approximately ten queries per second. We inspected sampled video frames and that primary explanation, not unpublished implementation code.

The transferable architecture is structured observations feeding typed decisions, followed by deterministic execution. This change implements the observation boundary and leaves model selection, decision policy, and fast control scheduling separate. It does not introduce Jev or claim equivalent reaction latency.

## Validation for this change

Focused validation passed: 7 native provider tests, 6 screen-publisher tests, 10 managed observation/transport tests, 10 Python helper tests, 8 example-producer tests, and the Lua addon mock assertions with JSON parsing. The native tests include helper cancellation, bounded output, timestamp checks, host/guest isolation, and a stalled provider preserving a successful screenshot.

On Omarchy, the Python provider consumed an archived real addon export through the new generic envelope: 36 prioritized elements included `Warming Up` and `Ready for turn-in`. A different app/window selector returned `context_mismatch`. The archived timestamp was deliberately preserved. The current Hand user has no desktop session bus, so its AT-SPI probe correctly returned `session_bus_unavailable`; a fresh live desktop-user AT-SPI tree is not claimed by these tests.

Top-level `observation.capturedAt` is the observation-request start anchor. Per-provider `capturedAt` describes that source's acquisition time. Neither field is an atomic screenshot timestamp. Current native VM publishers report provider unavailability rather than reading host context.

The code is developed in an isolated checkout. The existing main checkout has separate uncommitted computer-tool work; the shared screen schema/result functions are its integration point. This change has not been deployed or validated as a merged live computer-tool release.

## Wayland desktop publisher integration

The Go `hands/remote` publisher now uses the same provider contract and embedded Python collector on the agent completion path. Viewer video and frame captures do not collect semantic data. `nanocodex-remote observe-local` exercises that same read-only screenshot/provider path without a broker, using local environment configuration. The helper copies have a parity test.

Live Wayland validation found that AT-SPI `CoordType.SCREEN` may return window-local/logical bounds even on a scaled desktop. Results therefore label these `coordinateSpace: atspi_reported_screen` and `boundsVerified: false`. Consumers must reconcile bounds with compositor metadata or the screenshot before using them for input.

On September 17, the Go publisher was deployed to Omarchy's desktop-user session, and the managed tool transport was deployed from a current-base checkout. The live `computer.observe` response returned both a screenshot and attributed provider outcomes. WoW's login screen correctly returned `matching_window_unavailable`; this is not proof of an in-game addon capture. The publisher is a user service replacing the older publisher through the existing host-replacement protocol; the older system publisher remains idle as a fallback.
