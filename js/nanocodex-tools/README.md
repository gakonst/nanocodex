# nanocodex-tools

Reusable, platform-neutral JavaScript tools for Nanocodex agents: the common
tool router and Code Mode runtime, attachment and hosted-tool protocols,
durable-memory parsing and ranking, artifact and dataset tools, persistent
workspace adaptation, a bounded Just Bash runtime, Git/GitHub compatibility
commands, repository materialization, and workspace-backed SSH composition.
Session-search parsing, retrieval policy, and bounded model-visible projections
are exposed through the dedicated `nanocodex-tools/session` entrypoint.

Hosts own persistence, network policy, credentials, and socket transports and
inject those capabilities through the package's narrow interfaces.

`nanocodex` owns the Rust/WASM agent runtime and composes WASM, workspace, and
MCP adapters around these capabilities. It imports and reexports this package's
JS-only host capabilities; `nanocodex-tools` never imports `nanocodex`.

Cloudflare Workers may supply Durable Object persistence and WebSocket
registries to the hosted-tools core, but Cloudflare bindings, account authority,
Connect grants, and storage schemas remain owned by those Workers.

### Live hand screens

In the web agent terminal, open **Live view**, select a screen, and watch as
hand tools run. Pause, close, or hide the tab to stop capture. Landscape and
portrait images retain their aspect ratios. Viewing is read-only; agent actions
continue through the hand's tools.

JavaScript hosts opt in with an observation provider:

```js
import { createTools } from "nanocodex/tools";
import { createScreenObservation } from "nanocodex-tools/observation/node";

const tools = await createTools({
  attachmentId: "desktop",
  machines: [{ id: "desktop", name: "My desktop", workspace: process.cwd(), capabilities: ["screen"] }],
  tools: myComputerUseTools,
  observation: createScreenObservation({ source: "desktop" }),
});
await tools.attach(agent.toolsTarget()).connect();
```

For Android use `{ source: "android", device: "SERIAL" }`. Both providers require
FFmpeg. Desktop acquisition uses macOS `screencapture`, Linux X11, Windows
`gdigrab`, or `grim` on supported Wayland compositors. Android uses
`adb -s SERIAL exec-out screencap -p`. Grant screen-recording permission to the
host on macOS, and authorize the explicit Android device in adb.

Browser or other device adapters can implement
`{ surfaces, capture({ surfaceId, signal }) }`. Surface descriptors have `id`,
`name`, and `kind` (`desktop`, `browser`, or `phone`). Capture returns
`{ captured_at, width, height, mime_type, data }` with base64 JPEG or PNG data.
Honor abort signals and capture the actual surface controlled by the hand.
iOS and graphical VM displays require custom providers; built-in adapters for
those platforms are not included.

The managed SDK exposes the same capability:

```js
const [screen] = await agent.hands.list();
if (screen) {
  for await (const result of agent.hands.frames(screen, { signal })) {
    if (result.status === "frame") renderFrame(result.frame);
  }
}
```

Breaking iteration stops demand; abort the signal to interrupt an outstanding
request. After a reconnect, list again for the new route token. Pending requests
never move to a replacement hand.

Endpoints: `GET /v1/agents/:id/hands` and
`GET /v1/agents/:id/hands/frame?source=agent|account&route_token=...&surface_id=...`.
Both require the owning account with `agents:read` and `tools:use`. Connect grants
cannot enumerate or view screens. Route tokens select connections and are not
authorization credentials. Responses disable caching.

This is a live screenshot feed (up to four frames/second), without audio or
recording. Capture is limited to one request per hand every 250 ms, one capture
in flight, and a five-second deadline. Images are at most 180 KB; built-in
providers fit them inside 960×960. No capture runs without viewer demand. Frames
and capture requests never enter durable tool receipts, agent history, or replay
logs. Errors are redacted before transmission. Detaching, draining, replacing,
or revoking a hand cancels its observations independently of tool receipts.
