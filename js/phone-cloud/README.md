# Cloud phone runtime

`Dockerfile` builds a Linux native voice binary and runs the Node Twilio bridge
in a dedicated Cloudflare Container. `PhoneContainer` in the managed Worker
owns authentication, Twilio signature verification, runtime secret injection,
and the authoritative Durable Object SQLite call journal. Container disk is
only an ephemeral mirror. No workstation service is needed.

The container listens on port 8788 with internet egress enabled for native
WebRTC. Public HTTPS/WSS traffic enters through the account gateway at
`/v1/phone/bridge/*`. Internal state callbacks use the same protected gateway.
Startup hydration must remain callable while the container starts; do not wrap
container startup in a Durable Object concurrency barrier.

See [configuration and acceptance](../managed/PHONE.md). Cloud deployment and
live telephone audio must be verified separately from image build success.

## Setup status (2026-09-17)

The managed control routes, account gateway, and PhoneContainer are deployed.
Twilio confirms +16169716197 is an owned voice-capable number. The Auth Token,
`TWILIO_VOICE_FROM_NUMBER`, managed voice credentials, and bridge URL are configured
as Cloudflare secrets. The `phone` tool is enabled for the configured owner.

Cloud acceptance passed: protected setup returned the owned number and webhook
credential availability; `/health` returned `ready`; `/check` returned
`voice_ready` from the deployed Linux container (managed voice authentication,
ICE/DTLS connectivity, and voice protocol readiness). A user-authorized telephone
test subsequently confirmed two-way speech, transcript capture, and completion.
The workstation phone service is disabled.

Parallel calls are enabled with an internal bound of four voice sessions. Each
call retains independent media, transcript, and cleanup state. Preparing and
unknown calls retain capacity until reconciled. All 19 bridge tests passed,
including simultaneous signed media streams and independent hangup. Four
simultaneous silent cloud voice checks returned HTTP 200 / `voice_ready`. Two
user-authorized telephone calls then connected concurrently, exchanged intelligible
speech, retained separate transcripts, and completed successfully after hangup.
Four simultaneous live telephone calls have not yet been tested.

Container application: `a036a6cf-9380-4b49-881b-5a487301aade`.
Image digest: `sha256:784299c28444e68439a588b2948a01fc61d7171730d52fdc727955cd497095c9`.
The deployment preserved the existing Sandbox application without changes.

## Retained call agents and tool delegation

Each call now creates a dedicated retained managed agent and exposes
`call_agent_id` to its coordinating session. The phone voice delegates authorized
work through the existing managed voice lifecycle and receives the result over
its sideband connection. The owner brief defines the goal and authority; remote
speech is untrusted context. Call records retain transcripts, and agent threads
retain delegated work. Hangup stops delegation and requests cancellation while
preserving history. A nested call-thread UI and mid-call coordinator steering
are not implemented.

Verification: 100 focused JavaScript tests, managed typecheck, and 11 native phone
protocol tests passed. The deployed container returned HTTP 200 / `voice_ready`
with a successful public web lookup. Retained thread
`01a0b199-060a-7d9c-9c9c-c64526090dc8` records a completed `web__run` invocation,
one completed turn, and no active turns after cleanup. This verifies cloud tool
delegation; speaking a tool result during a real telephone call remains untested.
No new telephone calls were placed for this update.

Current phone image:
`sha256:5bbda5ce4cbd16199b4603d2fca6f2e62f8b9905b4f20c84144567b444615a16`.
