# Web screen discovery and reconnect recovery — 2026-09-16

The browser now warms the account-scoped screen catalog before opening Screens.
It keeps an existing connection through a background switch shorter than fifteen
seconds, releasing input immediately. A transient WebRTC disconnected state gets
three seconds to recover. Failed peers and closed sockets still reconnect.

A transient HTTP/network renewal failure retries after 500 ms while the original
authorization deadline remains in force. Only a fresh authenticated renewal can
extend that deadline; HTTP 401/403 and missing leases still detach immediately.
Closing the dialog or leaving the page still stops the connection immediately.

## Actual browser measurements

Chromium ran the actual RemoteScreens component with an account-context fixture
and the actual RemoteBrowserSession against production catalog, authorization,
and screen publishers through a local authenticated proxy. The app was idle for
two seconds before opening Screens. These are individual samples, not percentiles.

| WAN Linux desktop journey | Before | After |
| --- | ---: | ---: |
| Open Screens → list ready | 1,367 ms | 76 ms |
| Open Screens → desktop visible | 3,191 ms | 1,895 ms |
| Select desktop → desktop visible | 1,824 ms | 1,819 ms |

Discovery moved off the click path. The underlying cold screen handshake remains
network-bound; this change does not claim instantaneous cold WebRTC negotiation.

A 35-second actual frame-relay session then exercised a 500 ms background switch
and one synthetic HTTP 503 response to the first renewal. Before the fix, these
caused two reconnects, approximately 2.07 seconds of blank screen after returning
and another 2.92 seconds after the failed renewal. It opened three viewer sockets
and rendered 281 frames. After the fix, it kept one socket, rendered 332 frames,
and never left Watching during either interruption.

The Omarchy WebRTC publisher also stayed on one viewer socket for 65 seconds with
the same interruptions, decoding 3,675 frames. No input events were sent. Normal
baseline sessions on both publishers had no spontaneous reconnects over 65 seconds;
the failing conditions were deliberately reproduced rather than attributed to a
planned VM restart. Physical phone behavior has not been measured here.

## Published app verification

The account app was deployed from `e425cc33`, including master `9c072415`, as
Cloudflare version `482baaa8-714d-4086-851a-7056e23f813b`. Container rollout was
disabled. The published `index-BVm6TPRc.js` asset exactly matched the local build
(SHA-256 `bf27d5b1121adeb851e23bc3cebd9951e029e47092b74293a1c3a9b6a79c3b7a`).

A separate Chromium run loaded the full published app at `/connect` through the
same authenticated proxy, using the real account response and no component
fixture. After two idle seconds, the screen list appeared 45.8 ms after opening
Screens and the WAN Linux desktop appeared after 1,856.9 ms. Over the next
35 seconds, a synthetic 500 ms document-visibility change and one injected HTTP
503 renewal kept the same viewer socket open. The toolbar stayed Watching with
no intervening status changes, socket closes, or page errors. The renewal retry
and subsequent renewals returned 200; the canvas remained 1280 by 800.

## Verification

42 viewer tests cover the existing protocol and the new transient-disconnect,
short/long background pause, renewal retry, unchanged authorization deadline,
and immediate unauthorized/missing-lease rejection behavior. See the adjacent
[measurements](web-screen-recovery-measurements.json) for retained timings and
state transitions. No credentials, SDP, screenshots, or network addresses are retained.
