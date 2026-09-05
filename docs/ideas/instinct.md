# Instinct idea archive

Reference material from Instinct, kept as product inspiration rather than as
requirements. One-use tokens and other credentials are intentionally omitted.

## 2026-09-01 — Shareable Google Workspace connection link

Source: `https://app.instinct.co/connect/google-workspace` (shared via iMessage;
the access token has been removed)

### What it does

A service-specific link immediately starts Google OAuth instead of first routing
the recipient through a connector catalog. The authorization request bundles the
Google Workspace capabilities Instinct needs: Gmail, Calendar, Tasks, Drive,
Docs, Sheets, Slides, and read-only Contacts.

The link is delivered as a native rich preview inside an iMessage conversation.
The user says “let's connect gmail”; the assistant replies that it takes one tap,
notes that Calendar, Drive, and Docs can be used later, and sends a branded
“Connect Google to Instinct” card. Connection is therefore part of the chat
rather than a separate settings journey.

Google then presents one granular consent screen for the whole account. The user
can select all permissions or choose individual capabilities: full Drive access,
read-only Contacts, Slides, Docs, Sheets, Calendar event editing, Calendar read
access, Gmail settings and filters, Gmail read/compose/send, and Tasks. The
account identifier visible in the reference screenshot is intentionally not
retained here.

### Contrast with the current Nanocodex model

Instinct exposes one user-facing **Google Workspace** connection, while the
provider's consent screen supplies the fine-grained permission selection.
Nanocodex currently presents Google services such as Gmail and Google Drive as
separate connections. The Instinct approach makes initial connection feel like
one action without forcing an all-or-nothing grant: provider identity is bundled,
but capabilities remain selectable.

Potential model for Nanocodex:

1. Show one Google account connector in the conversation or account UI.
2. Project the complete requested capability set before leaving Nanocodex.
3. Let the user narrow those capabilities on Google's native consent screen.
4. Return with one connected Google identity plus an explicit record of which
   services and scopes were actually granted.
5. Ask for incremental authorization later when an agent needs an ungranted
   capability, rather than creating a second conceptual account connection.

### Channel-aware completion and identity

After OAuth, Instinct shows a sparse success page stating that **Google
Workspace is connected**. Its primary CTA is **Return to iMessage**, including
the Messages icon; **Open workspace** is a quieter secondary action. This closes
the loop in the surface where the connection request began instead of depositing
the user in a generic dashboard.

The reference also shows that Instinct has different phone numbers for WhatsApp
and iMessage. The assistant name and product identity stay consistent, but each
transport has its own business/contact identity and platform-native trust
surface. The phone numbers visible in the screenshots are intentionally not
retained here.

Potential model for Nanocodex:

1. Bind each connection request to its originating installation, conversation,
   and channel—not merely to a generic source label.
2. After callback validation, show the exact account bundle and granted
   capabilities that became connected.
3. Make the primary completion action a safe deep link back to the originating
   conversation; retain the Nanocodex web workspace as a fallback.
4. Give each messaging transport an explicit ingress identity and trust model
   while resolving them to the same Nanocodex account or agent where intended.
5. Never infer that two phone identities belong to the same user or agent solely
   because their display names match; link them through an authenticated account
   ceremony.

### Agent-owned email address

Every Instinct agent receives an email address on Instinct's domain. The concrete
address visible in the reference is intentionally not retained here.

This is different from connecting the user's Gmail account. A Gmail connector
lets an agent act through the user's identity and permissions; an agent-owned
address gives the agent its own durable public endpoint. Users and services can
send it messages, documents, receipts, invitations, and follow-up requests
without granting access to a personal mailbox.

Potential model for Nanocodex:

1. Provision a stable inbound email identity for each agent, with an explicit
   owning account and lifecycle.
2. Route messages, threads, and attachments into the same durable agent history
   used by its other channels while preserving email-specific metadata.
3. Treat all inbound mail as untrusted external input. Fence prompt injection,
   spoofed senders, malicious links and attachments, and cross-account routing.
4. Distinguish received identity from verified identity; display names and From
   headers alone must not authorize actions or reveal account data.
5. Make outbound email policy explicit: allowed recipients, approval boundaries,
   rate limits, unsubscribe handling, abuse controls, and a complete audit trail.
6. Authenticate the sending domain and expose delivery, bounce, and complaint
   state without making provider credentials available to agents.

### Connection and capability catalog shown in chat

- Google: Gmail, Calendar, Drive, Docs, Sheets, Slides, Tasks, Contacts.
- Microsoft: Outlook mail.
- Work: Slack, Linear, Notion, GitHub, Granola meeting notes.
- Messaging: the user's WhatsApp through Linked Devices, plus Instinct-owned
  iMessage, WhatsApp, Slack, and email lines.
- Life: flights, maps, weather, Yelp, Stripe Link, and a credential vault for
  checkout.
- Cloud browser: saved-login browser automation for services without APIs.

The catalog is paired with candid limitations: nothing begins connected; phone
calls still require the user on the line; group chats are not supported; plain
SMS/RCS is not trusted like iMessage; several major consumer and work services
are absent; browser automation is slower and more fragile than APIs; and memory
starts fresh rather than silently importing signup history.

### Personal action vault

Instinct has a first-class **Vault** beside the main **Workspace**. The empty
state exposes four structured categories:

- Logins
- Payment cards
- Addresses
- Phone numbers

This is distinct from OAuth connectors. Connectors grant API capabilities;
vault entries supply user-owned credentials, payment methods, and form-fill
identity for browser-mediated tasks such as purchases, reservations, and sites
without APIs. The sparse UI makes each sensitive item an explicit user addition
rather than something inferred from conversation history.

Potential model for Nanocodex:

1. Treat the vault as its own security and data-ownership boundary, not as agent
   memory or ordinary connector configuration.
2. Give agents opaque references and bounded actions instead of returning raw
   passwords, complete card details, or reusable personal data to model context.
3. Inject secrets only at the trusted egress or browser boundary and reveal the
   minimum fields needed for the approved action.
4. Require an exact approval projection for consequential use: merchant or site,
   selected payment method, amount when known, shipping identity, and intended
   operation.
5. Support independent listing, replacement, revocation, and audit history for
   each vault item without disconnecting unrelated accounts or agents.
6. Keep the vault useful across agents and channels while fencing access by
   account, installation, grant, and action.

### Ideas for Nanocodex

- Make a connector request shareable: opening the link should explain who is
  requesting access, show the exact projected capabilities, and lead directly
  into connection.
- Let the agent create and send the focused connection action in the conversation
  where the need emerges. Avoid making the user hunt through settings first.
- Follow a long capability explanation with one prioritization question: identify
  the account currently costing the user time, then connect only that one.
- Be explicit about the difference between potential capability, connected
  capability, API-backed reliability, and browser-automation fallback.
- Preserve the originating surface (for example, iMessage) so the completion
  flow can return the user to the right conversation or installation.
- Make the OAuth success page channel-aware: confirmation first, return-to-chat
  as the primary CTA, and the web workspace as a secondary escape hatch.
- Separate the assistant's stable product identity from its channel-specific
  addresses, business profiles, delivery guarantees, and trust boundaries.
- Give an agent its own address where a durable public inbox is useful; do not
  require access to the user's personal email merely to receive agent-directed
  correspondence.
- Separate OAuth/API connections from a personal action vault. Both enable tools,
  but they have different consent, disclosure, revocation, and audit semantics.
- Prefer structured, user-maintained checkout identity over teaching the agent to
  recover addresses, phone numbers, payment data, or passwords from chat memory.
- Treat “Google Workspace” as a convenient product bundle while retaining
  least-privilege grants and explicit service boundaries underneath it.
- Separate the user-facing account connection from the internal capability
  grants: one Google identity can back independently granted Gmail, Drive,
  Calendar, Contacts, Docs, Sheets, Slides, and Tasks capabilities.
- Reflect partial consent accurately after OAuth. A successful Google connection
  must not imply that every offered capability was granted.
- Keep provider credentials and one-use connection state out of URLs that are
  retained in product history, logs, analytics, or documentation.

### Questions to revisit

- Should shared links request a fixed bundle, or let the recipient narrow the
  requested Google services before authorizing?
- Should the Connect approval projection show the service bundle, the exact
  provider scopes, or both—and how should it reconcile the final partial grant?
- Should the link connect a personal account, approve access for a specific
  agent or installation, or perform both as separate signed steps?
- What signed return-state is needed to restore the exact channel and
  conversation without allowing an arbitrary redirect or cross-account handoff?
- How should one agent advertise and manage distinct iMessage, WhatsApp, Slack,
  email, and other channel identities?
- Is an email address owned by the agent, an installation, or the account—and
  what happens to it when agents are cloned, shared, transferred, or deleted?
- Which inbound email events may wake or steer an agent, and which require user
  review before entering its trusted working context?
- Which vault operations may run unattended, which require per-use approval, and
  which should never be available to an agent?
- Can browser autofill remain end-to-end opaque to the model while still
  providing useful progress, failure, and receipt evidence?
- What should happen when some services are already connected or the recipient
  lacks permission to grant one of them?
