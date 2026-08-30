# Nanocodex social launch copy

This copy follows Georgios's public framing. It stays concrete and first-person,
focused on frontier infrastructure people can own, and willing to state the
architecture in one line. The blog carries the qualifications and evidence;
the first post should make the product legible.

## Single post

Introducing Nanocodex Managed.

Create a durable Codex agent over an API. Connect a user’s ChatGPT subscription
and tools. Attach its output to web, Slack, or mobile. Detach and the work keeps
running; reattach from the last cursor and nothing is lost.

We are also open sourcing Nanocodex, the Rust/WASM runtime underneath it.

We turned the harness inside out: durable state lives outside the process, and
sandboxes are tools the agent attaches only when needed.

Start hosted on Paradigm. Export the runnable journal to Postgres, Cloudflare,
Vercel, or your own host.

Open source. Frontier agentic infrastructure you can actually own.

## Thread

### 1

Introducing Nanocodex Managed: durable Codex agents you can embed in any
product, with Nanocodex as the open-source Rust/WASM runtime underneath.

Create an agent over the API. Connect a user’s ChatGPT subscription and tools.
Attach its output to web, Slack, mobile, or several clients at once.

### 2

Detach and the work continues. Reattach after the last durable cursor and
nothing is lost.

An agent is not a long-running HTTP request. It is a durable object with many
possible observers.

### 3

Agents are following the path of embedded wallets: moving from a separate
destination into the products where people already work.

The product owns the experience. The agent brings durable state and explicit
capabilities. Embedded should not mean captive.

### 4

Centaur proved that stock Codex + tools + durability could become shared
infrastructure for a company.

It also showed us the limit: the harness lived inside the sandbox, so every
agent inherited the lifecycle and cost of a machine.

Centaur was right about the harness and wrong about the boundary.

### 5

Nanocodex turns that boundary inside out.

Nanocodex embeds the agent as a library. Managed hosts the same runtime behind
an API. A portable journal owns committed state. Just Bash, browsers, VMs,
private workers, and MCP servers are hands attached when the task needs them.

### 6

Most agent work does not need a full machine at the first token.

Start immediately with a durable lightweight workspace. If the task needs a
native package, browser, GPU, private network, or stronger isolation, attach
the right hand and continue the same turn.

### 7

Users can bring an eligible ChatGPT subscription for Codex model access. No
OpenAI API key needs to enter the product.

Connect also grants GitHub, Google, MCP, and other tools. Credentials stay
behind the broker, outside the agent, generated code, and sandbox.

### 8

Nanocodex Managed gives you the full lifecycle over one API: idempotent agent
and turn admission, ordered durable events, cursor reconnect, steering,
cancellation, and multi-client projection.

Close the laptop. The work continues.

### 9

Hosted does not mean trapped.

Export the runnable Rust-owned journal, replicate from its cursor, fence the
source, and import it into Postgres, Cloudflare, Vercel, or another compatible
host. Reauthorize secrets at the destination and resume the same agent.

### 10

Nanocodex is intentionally narrow: one OpenAI coding stack, faithfully
implemented; your interface, data, tools, infrastructure, and policy.

The library is open source. The managed API and Connect are hosted by Paradigm.

[blog link]
[repository link]
