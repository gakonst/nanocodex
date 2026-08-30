# Nanocodex social launch copy

## Single post

Introducing Nanocodex Managed.

Embed a durable Codex agent in any product. Let users bring their ChatGPT
subscription and connected accounts. Keep the agent alive without paying for
an idle container; attach browsers, sandboxes, GPUs, and private workers only
when needed.

Start hosted on Paradigm. Export the runnable state to Postgres, Cloudflare,
Vercel, or your own Nanocodex deployment.

We are also open sourcing the Rust/WASM agent and durability runtime underneath
it.

## Thread

### 1

Introducing Nanocodex Managed: durable Codex agents you can embed in any
product.

Create one over an API. Put it inside web, Slack, mobile, or a background job.
Disconnect and the work continues. Reconnect from the last durable event.

### 2

Four things are different:

1. Embed the agent instead of redirecting the user.
2. Connect accounts once and grant them across products.
3. Keep the durable brain; rent execution hands when needed.
4. Start hosted and leave with runnable state.

### 3

Embed is the product layer.

The application owns the interface and policy. Nanocodex owns the Codex
lifecycle, durable output, reconnection, and recovery. Several clients can
attach to the same agent without any of them becoming its owner.

### 4

Connect is the account and authorization layer.

Users can bring an eligible ChatGPT subscription, GitHub, Slack, Google,
wallets, MCP servers, and private machines. A connection added in one embedded
product is available in another under a new explicit grant.

Credentials stay behind the egress broker. They never enter the application,
generated code, agent, or sandbox.

### 5

The managed agent is the durable brain. Capabilities are its hands.

The brain does not require a permanent container. Common work starts in the
Rust/WASM runtime. Attach a browser, sandbox, VM, GPU, or reverse-connected
private worker only when the task needs it.

The agent can live for weeks. The expensive machine can live for minutes.

### 6

Hands are not tied to the agent's host. They can run inside the product, behind
Connect, on a laptop, in a private network, or with a compute provider.

Mercator handles discovery and routing. MPP handles metering and payment under
the spending authority granted through Connect, with Tempo available for
settlement.

### 7

Hosted does not mean captive.

Export the Rust-owned journal, replicate from its cursor, fence the old writer,
and resume the agent on Postgres, Cloudflare, Vercel, or another compatible
Nanocodex deployment.

This is runnable state, not a transcript download. Secrets stay behind and are
reauthorized at the destination.

### 8

Centaur proved that Codex becomes company infrastructure when it can work for a
long time, use real systems, and live where people already work. It also showed
the cost of a permanent container per agent.

Nanocodex is the version we can offer to every developer. It is embedded,
connected, cheap while idle, and portable.

Apache/MIT. Rust, JavaScript/WASM, and Python.

[blog link]
[repository link]
