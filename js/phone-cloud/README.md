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

## Retained call agents and tool delegation

Each call creates a dedicated retained managed agent and exposes
`call_agent_id` to its coordinating session. The phone voice delegates authorized
work through the existing managed voice lifecycle and receives the result over
its sideband connection. The owner brief defines the goal and authority; remote
speech is untrusted context. Call records retain transcripts, and agent threads
retain delegated work. Hangup stops delegation and requests cancellation while
preserving history. A nested call-thread UI is not implemented.

## Call coordination

The browser chat groups calls made by the current parent agent. Each row shows
its destination, lifecycle status, transcript, and retained call-agent link.
Visible chats refresh the list every four seconds; hidden chats stop polling.
The `phone` tool also supports `list` for the current parent.

`steer` accepts `call_id`, a stable UUID `operation_id`, and owner instructions
(up to 8,000 UTF-8 bytes). Instructions amend the original brief and preserve
constraints unless explicitly changed. The bridge journals each request before
delivery, rejects conflicting reuse, and fences stale delegated answers.
A `submitted` receipt means the voice process received the update, not that
the model acknowledged it. Reconcile uncertain requests with the same operation
ID and instructions. Steering never starts or redials a call. Hangup remains
available from the panel and tool.

The authenticated `/v1/agents/:id/phone/calls` routes enforce the parent account
and deployment-selected phone admin. These controls require the updated cloud
phone container as well as the managed Worker and browser application.
