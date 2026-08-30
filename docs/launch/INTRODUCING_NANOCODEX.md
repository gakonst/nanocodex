# Introducing Nanocodex

*Managed Codex agents you can embed in any product, connect to a user's accounts and machines, and move off our cloud.*

By Georgios Konstantopoulos

Today we are releasing Nanocodex Managed, Connect, and the open-source Nanocodex agent runtime.

Nanocodex gives developers a long-running Codex agent over an API. The agent keeps working when the client disconnects and can appear inside a web application, Slack, a mobile client, or a background job.

Four things are different:

1. **Embed, do not redirect.** The agent lives inside the product where the work starts. The product owns the interface; Nanocodex owns the agent lifecycle.
2. **Connect once, grant anywhere.** A user can bring their ChatGPT subscription, accounts, wallet, and private machines. Those connections follow the user's Nanocodex account across embedded products, while each product receives only the access the user approved.
3. **Keep the brain; rent the hands.** The durable agent does not sit inside a permanent container. It attaches a browser, sandbox, GPU, API, or private worker only when the work needs one.
4. **Hosted does not mean captive.** Nanocodex exports runnable agent state, not only transcripts. The same agent can resume on Postgres, Cloudflare, Vercel, another Nanocodex deployment, or infrastructure you operate.

The stack is:

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

## Embed

Agents are moving from terminals and dedicated chat applications into the software where people already work.

The agent can work beside a research document, inside a support ticket, or against a development repository. The user should not have to leave the product, open another application, and reconstruct the context.

Nanocodex exposes the same managed agent through JavaScript, React, and an HTTP API. A browser tab, Slack thread, phone, and server process can attach to one running agent. Closing one client detaches that client; it does not cancel the work. Another client can open the agent later and continue from the last durable event it received.

The application can also give the agent native access to the product. Its own functions can be local tools. A browser can expose the current page through WebMCP. The agent can use the product's actions and current context directly instead of scraping the interface or moving the product's data into a remote machine.

This is similar to the shift from standalone wallets to embedded wallets. The application owns the experience while specialized infrastructure handles keys and authorization. Agents need the same separation for identity, connected accounts, durable work, and execution.

## Connect

Connect is the identity and authorization layer for embedded agents.

A user can connect GitHub, Slack, Google, Linear, a ChatGPT subscription, a wallet, or a private machine to their Nanocodex account. These connections belong to the account rather than to one agent or application.

When the same account appears in another Nanocodex-powered product, its connections are already available. The new product asks for a scoped grant covering the agent, tools, data, output visibility, and optional spending authority it needs. Connecting GitHub in one product should not require connecting GitHub again in every other product that embeds an agent.

The reverse is also useful. If a user connects a new account from one embedded product, that capability becomes available anywhere else they use the same Nanocodex account, subject to a new explicit grant. Connect turns a collection of OAuth integrations, wallets, machines, and model access into a portable capability graph.

Connect works with Nanocodex accounts and with applications that already use Auth0, Better Auth, Privy, or another identity provider. The application keeps its existing login. It links its user to a Nanocodex account and opens the Connect flow when it needs agent capabilities.

Credentials stay behind the broker. The application, generated code, agent, and sandbox receive a capability grant, not an OAuth refresh token or SSH private key. The egress layer checks the grant and injects the current credential at the network boundary. Revoking a connection or grant takes effect without finding and deleting credential copies from running machines.

ChatGPT model access uses the same mechanism. A user can connect an eligible ChatGPT subscription for Codex without giving the embedding application an OpenAI API key. Paradigm stores and refreshes the account credential and uses it only at the fixed Codex transport boundary. Usage remains subject to the user's OpenAI plan, limits, and terms.

## Managed Agents and Hands

A managed agent is the durable brain. Capabilities are its hands.

The brain owns the Codex context, ordered prompt queue, compaction, tool calls, steering, cancellation, branching, and committed output. Clients can attach and detach without becoming responsible for that state. Applications do not replay message history, preserve provider response IDs, or guess whether work completed after a connection failure.

The hands can live anywhere. They can be functions inside the application, OAuth-backed APIs behind Connect, actions exposed by the browser, lightweight filesystem and shell tools, hosted sandboxes, GPUs, laptops, or workers inside a private network. A private worker connects outward and registers its tools without accepting public inbound traffic.

The split is what makes the cost model work.

Most managed-agent systems start by assigning every agent a container. The container then sits around while the model reasons, waits for the user, calls an API, or does nothing. Keeping the agent alive means keeping the machine alive.

Nanocodex does not require a machine per agent. The durable brain can remain available without an idle VM. Common filesystem, shell, API, and product-local work runs in the Rust or WASM runtime. If a task needs a native binary, browser, stronger isolation, more compute, or a GPU, Nanocodex attaches that hand for the task and retains it only while useful.

The agent can live for weeks. The expensive machine can live for minutes.

[Anthropic describes this as separating the brain from the hands](https://www.anthropic.com/engineering/managed-agents). Nanocodex applies the same operational model to Codex, but the hands also include product-local tools, connected accounts, reverse-attached private workers, and paid capabilities discovered at runtime.

We care about matching Codex rather than hiding it behind a generic model abstraction. Prompt shape, tool schemas, result ordering, caching, compaction, retry behavior, and process cleanup affect performance on long tasks. Nanocodex tracks the behavior of Codex and ports the parts required to reproduce it as a library.

## Capabilities, Mercator, and MPP

A capability is any hand the agent can call: a function in the application, an OAuth-backed API, a browser, a sandbox, a GPU, a private worker, a dataset, or a specialized service.

Connect determines which capabilities the user and application may access. Mercator handles discovery and routing across available providers. The [Machine Payments Protocol](https://tempo.xyz/solutions/agentic-payments/), co-authored by Stripe and Tempo, handles payment negotiation and metering for capabilities that cost money. Payments can settle on Tempo.

An agent can start with inexpensive local execution, discover that it needs a native binary or more compute, select an eligible provider through Mercator, and pay for the resource over MPP within the spending authority the user approved in Connect.

The same model applies to underused resources. A developer can expose a machine or service as a capability, set its policy and price, and let eligible agents find it. The agent does not need to know which vendor owns the sandbox or where the worker runs. It needs a typed capability, a grant, and a route.

## Open durability

Managed agents should not create another cloud silo.

Downloading a transcript is insufficient. A transcript describes what happened but usually cannot resume an interrupted agent. Nanocodex exports the ordered operations and committed agent history, along with the checkpoints, effect identities, outputs, and recovery metadata required to continue running.

The open-source durability library defines that journal and the rules for reducing it into agent state. Storage adapters implement atomic load and compare-and-append without redefining the format. The same journal can live in memory, SQLite, Postgres, a Cloudflare Durable Object, a Vercel Workflow, or Paradigm's hosted service.

A team can start on Nanocodex Managed, export a consistent snapshot, and resume against Postgres it operates. It can import the same state into Cloudflare, Vercel, or another Nanocodex deployment. It can also follow an incremental cursor and replicate state before a migration.

Secrets are excluded. The destination must reauthorize Connect and bind the tools and compute it intends to use. Durable agent state moves; credentials do not.

Paradigm hosts the default service because most developers should not have to operate this infrastructure. The open journal and adapters ensure that using the hosted product does not give Paradigm the only runnable copy of the agent.

## What Centaur proved

We built Nanocodex after operating [Centaur](https://www.paradigm.xyz/writing/open-sourcing-centaur-multiplayer-self-hosted-secure-agents) across Paradigm and Tempo.

Centaur's core decision was simple: use Codex rather than replace it with a third-party harness. Put it where people already work, let sessions run for a long time, and connect it to real company systems. By [Centaur 2.0](https://www.paradigm.xyz/writing/centaur-2-0-permissions-context-and-mcp), more than 80% of sessions happened in shared channels and more than 99% of daily sessions completed successfully.

It proved that embedded, durable, connected agents are useful. It also made the cost of a permanent container per session obvious. Nanocodex keeps the part that worked—the Codex agent—and makes the expensive execution environment an optional hand.

Nanocodex Managed is the version of that product we can offer to every developer without asking them to reproduce the infrastructure behind Centaur.

## Available today

Nanocodex is open source under Apache 2.0 or MIT. It includes the Rust agent, JavaScript and Python bindings, the WASM runtime, tools, durability adapters, and deployment examples.

Nanocodex Managed provides the hosted agent API, Connect, durable storage, secure egress, capability routing, and sandbox lifecycle. Mercator and MPP let agents discover and pay for additional capabilities under user-approved limits, with Tempo available as the settlement rail.

Embed the agent in your product. Let the user bring their accounts. Pay for the hands only when the work needs them. Leave with the runnable state if you want to operate it yourself.

Code and documentation are available on [GitHub](https://github.com/gakonst/nanocodex).
