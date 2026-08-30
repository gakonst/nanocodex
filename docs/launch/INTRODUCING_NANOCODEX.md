# Introducing Nanocodex

*Managed Codex agents you can embed in any product, connect to a user's accounts and machines, and move off our cloud.*

By Georgios Konstantopoulos

Today we are releasing Nanocodex Managed, Connect, and the open-source Nanocodex agent runtime.

Nanocodex Managed gives developers a long-running Codex agent over an API. The agent keeps working when the client disconnects and can appear in a web application, Slack, a mobile client, or a background job. Connect gives that agent access to the user's existing accounts, ChatGPT subscription, tools, and private machines without passing reusable credentials into the application or its sandbox.

We built this after operating [Centaur](https://www.paradigm.xyz/writing/open-sourcing-centaur-multiplayer-self-hosted-secure-agents) across Paradigm and Tempo. Centaur proved that Codex becomes much more useful when it can work for a long time, use real company systems, and meet people where they already work. It also showed us that putting the agent, its state, and its tools inside a permanent sandbox becomes expensive and difficult to operate at scale.

Nanocodex separates them. The agent is durable without owning a machine. Sandboxes, browsers, APIs, laptops, and private workers are capabilities it can attach when needed. The user's identity and connected accounts live above the agent and can be reused anywhere the agent is embedded. The durable state beneath it is open and exportable.

The stack looks like this:

```text
Embed
JavaScript · React · API · Slack · web · mobile

Connect
Identity · OAuth · ChatGPT · wallet · grants · spend permissions

Managed Agents
Codex · context · long-running work · attach / detach

Capabilities
Tools · APIs · browsers · compute · private workers · paid services
Mercator discovery and routing · MPP payments settled on Tempo

Open Durability
Paradigm · Postgres · Cloudflare · Vercel · your cloud
```

Each layer can be used independently. Together they are the hosted product we wanted while building Centaur.

## From Centaur to Nanocodex

Centaur began with a simple decision: use Codex rather than replace it with a third-party agent harness.

We put Codex in a sandbox, connected it to Slack, gave it durable execution and permissioned access to company tools, and let people share sessions. One Slack thread mapped to one agent. The agent could work for hours, survive restarts, and hand control between several people. By the time we released [Centaur 2.0](https://www.paradigm.xyz/writing/centaur-2-0-permissions-context-and-mcp), more than 80% of sessions happened in shared channels and more than 99% of daily sessions completed successfully.

The product worked. The implementation was painful.

Every agent lived inside a Linux container with a CLI harness. Keeping an agent available meant keeping track of a machine and a process. We built process supervision, output capture, reconnection, recovery, remote tool transport, credential brokering, and sandbox lifecycle around it. Tasks that needed an API call or a small filesystem still paid the cost of a general-purpose sandbox. Tools inside a private network required us to move the harness toward the tool.

The agent, its durable state, and its execution environment have different lifecycles. Treating them as one object made each of them harder to operate.

[Anthropic reached a similar conclusion](https://www.anthropic.com/engineering/managed-agents) while scaling its managed agents. Their brain-and-hands model separates the session and harness from the sandbox, then provisions execution when the agent needs it. Nanocodex applies that architecture to Codex and takes it further with a portable identity layer, runtime capability discovery and payment, and an open durability format.

## Embed

An agent should be part of the product where the work begins.

Nanocodex exposes the same managed agent through JavaScript, React, and an HTTP API. A Slack thread, browser tab, phone, and server process can attach to one running agent. Closing one client does not stop the turn. Another client can open the agent later and continue from the last durable event it received.

The application owns the interface and product policy. Nanocodex owns the agent lifecycle, ordered output, reconnection, and recovery.

This follows the path of embedded wallets. Wallet infrastructure moved account creation, keys, and authorization into applications while letting each application design its own experience. Agents are moving from terminals and dedicated chat products into the software where people already work. They need an equivalent account and authorization layer, which is what we built with Connect.

Embedding is also how an agent learns the product it is inside. An application can expose its own actions as local tools or WebMCP capabilities. The agent can operate the product directly without scraping its interface or requiring the product to move its data into a remote sandbox.

## Connect

Connect is the identity and authorization layer for embedded agents.

A user can connect GitHub, Slack, Google, Linear, a ChatGPT subscription, a wallet, or a private machine to their Nanocodex account. These connections belong to the account rather than to one agent or application. When the same account is used in another Nanocodex-powered product, its connections are already available. The new product asks for a scoped grant covering the agent, tools, data, output visibility, and optional spending authority it needs.

Connecting GitHub in one product should not require connecting GitHub again in every other product that embeds the same agent account.

Connect works with Nanocodex accounts and with applications that already use Auth0, Better Auth, Privy, or another identity provider. The application keeps its login. It links its user to a Nanocodex account and invokes the same grant flow when it needs agent capabilities.

Credentials stay behind the broker. The application, generated code, agent, and sandbox receive a capability grant, not an OAuth refresh token or SSH private key. The egress layer checks the grant and injects the current credential at the network boundary. Revoking a connection or grant takes effect without finding and deleting copies from running sandboxes.

ChatGPT model access uses the same mechanism. A user can connect an eligible ChatGPT subscription for Codex without giving the embedding application an OpenAI API key. Paradigm stores and refreshes the account credential and uses it only at the fixed Codex transport boundary. Usage remains subject to the user's OpenAI plan, limits, and terms.

Connect makes the user's tools follow the user while keeping authorization explicit per application.

## Managed Agents

Nanocodex Managed is a hosted Codex agent with durable context and output.

The service creates the agent, accepts turns, records committed work, and lets clients attach or detach. Applications do not replay message history, preserve provider response IDs, or guess whether a request completed after a connection failure. The agent owns its ordered prompt queue, context, compaction, tool calls, steering, cancellation, branching, and cleanup.

We care about matching Codex rather than hiding it behind a generic model abstraction. Models and their harnesses are developed together. Prompt shape, tool schemas, result ordering, caching, compaction, retry behavior, and process cleanup all affect performance on long tasks. Nanocodex tracks the behavior of Codex and ports the parts required to reproduce it as a library. Supporting every model through one lowest-common-denominator loop is outside the scope of the project.

Separating the agent from the sandbox changes the cost model. An idle agent does not need an idle VM. The agent can begin in the Rust or WASM runtime and use lightweight filesystem, shell, API, and product-local tools. A container, browser, VM, GPU, or customer machine is attached when the task requires it and retained only while useful.

The agent's lifetime can be weeks. The expensive machine's lifetime can be minutes.

## Capabilities, Mercator, and Tempo MPP

We use capability to mean any hand the agent can call: a function in the application, an OAuth-backed API, a browser, a sandbox, a GPU, a private worker, a dataset, or a specialized service.

Capabilities do not need to run beside the agent. A hosted tool can expose HTTPS or MCP. A laptop or worker inside a private network can connect outward to Nanocodex and register tools without accepting public inbound traffic. A browser can expose the actions of the current product. A sandbox can appear for one task and disappear after its filesystem has been committed.

Connect determines which capabilities the user and application may access. Mercator handles discovery and routing across available providers. The [Machine Payments Protocol](https://tempo.xyz/solutions/agentic-payments/), co-authored by Stripe and Tempo, handles payment negotiation and metering for capabilities that cost money. Payments can settle on Tempo.

This lets an agent acquire resources during a turn. It can start with inexpensive local execution, discover that it needs a native binary or more compute, select an eligible provider through Mercator, and pay for the resource over MPP within the spending authority the user approved in Connect.

Centaur gave every agent a fixed set of hands inside its sandbox. Nanocodex lets the agent attach the right hand when it needs it.

## Open durability

Managed agents should not create another cloud silo.

Downloading a transcript is insufficient. A transcript describes what happened but usually cannot resume an interrupted agent. Nanocodex exports the ordered operations and committed agent history, along with the checkpoints, effect identities, outputs, and recovery metadata required to continue running.

The open-source durability library defines that journal and the rules for reducing it into agent state. Storage adapters implement atomic load and compare-and-append without redefining the format. The same journal can live in memory, SQLite, Postgres, a Cloudflare Durable Object, a Vercel Workflow, or Paradigm's hosted service.

A team can start on Nanocodex Managed, export a consistent snapshot, and resume against Postgres it operates. It can import the same state into Cloudflare, Vercel, or another Nanocodex deployment. It can also follow an incremental cursor and replicate state before a migration.

Secrets are excluded. The destination must reauthorize Connect and bind the tools and compute it intends to use. Durable agent state moves; credentials do not.

Paradigm hosts the default service because most developers should not have to operate the machinery we built for Centaur. The open journal and adapters ensure that using the hosted product does not give Paradigm the only runnable copy of the agent.

## What is different

Nanocodex Managed makes four concrete tradeoffs:

1. **Execution is attached on demand.** The agent can persist without paying for a permanent sandbox.
2. **Capabilities can live anywhere.** Local code, connected APIs, browsers, hosted compute, and private workers use the same tool boundary.
3. **Identity follows the user.** Connections made through Connect can be granted to agents embedded in other products without repeating setup or exposing credentials.
4. **Durable state is portable.** Users can export runnable state and continue on another supported host.

We chose Codex compatibility over model-provider abstraction, explicit grants over ambient credentials, and an open journal over a provider-owned workflow database.

## Available today

Nanocodex is open source under Apache 2.0 or MIT. It includes the Rust agent, JavaScript and Python bindings, the WASM runtime, tools, durability adapters, and deployment examples.

Nanocodex Managed provides the hosted agent API, Connect, durable storage, secure egress, capability routing, and sandbox lifecycle. Mercator and MPP let agents discover and pay for additional capabilities under user-approved limits, with Tempo available as the settlement rail.

Centaur showed us the product. Nanocodex is the stack we needed to run it everywhere.

Code and documentation are available on [GitHub](https://github.com/gakonst/nanocodex).
