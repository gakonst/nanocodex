# Private browser continuation

The first Vault browser implementation could submit username/password forms but
intentionally quarantined the whole browser session afterwards. Closing that
session destroyed its authentication state. It was therefore not an end-to-end
login-and-browse implementation.

## Boundaries

Ordinary `browser_execute` stays blocked after credential injection. The private
host connection is the only path for continuation. It may return a bounded,
redacted view of visible content, not raw DOM, input values, network responses,
cookies, storage, JavaScript results, or provider connection details. Navigation
and actions remain tied to the approved HTTPS origin and target. Website content
is untrusted data, not authority to use another credential or perform a purchase.

Verification codes belong to a client-owned form. The model requests an opaque,
short-lived challenge and receives only metadata. The client sends the code
directly to an authenticated endpoint. The challenge binds the owner session,
browser session, target, origin, and current document. Consume it before attempting
submission; an ambiguous failure must not silently resubmit the code. Code values
must not appear in conversation events, logs, durable storage, or receipts.

A submitted form and the absence of login inputs are never proof of success.
Status must distinguish a recognized verification form from unsupported pages;
readable account-page evidence is needed before claiming authenticated access.
CAPTCHA and human approval gates require actual user control of the same browser,
not a direction to open a different browser on the user's phone.

## Validation requirements

- Password -> verification -> account fixture, retaining the same browser session.
- Challenge expiration, single use, document changes, wrong account/session,
  missing permissions, Connect grants, malformed and oversized requests.
- Redaction of password/username echoes and verification-code-like strings;
  no input values, hidden text, raw URLs, scripts or arbitrary attributes.
- Cross-origin navigation/actions rejected; ordinary CDP quarantine persists
  across reloads and Worker rehydration.
- Browser state survives private reads and navigation; close remains an explicit
  destructive reset, not the way to finish login.
- Client code entry is transient, secure, and never emitted as conversation text.

## agent-browser comparison (2026-09-17)

Official sources reviewed:

- https://agent-browser.dev/security — encrypted auth profiles, selectors, same-page
  login and exact URL checks; opt-in action and domain controls.
- https://agent-browser.dev/streaming — viewport and mouse/keyboard/touch transport.
- https://agent-browser.dev/sessions — persistent authenticated browser sessions;
  exported state is sensitive and is not part of this implementation.
- https://agent-browser.dev/dashboard — dashboard feeds include console and command
  output; these must not be reused for our human-only private view.
- https://github.com/vercel-labs/agent-browser/blob/main/skill-data/core/references/authentication.md
  — manual 2FA and application-specific post-login verification.
- https://github.com/vercel-labs/agent-browser/issues/1836 — staged fresh TOTP is
  proposed, not established shipped support.

The useful architectural additions are retained authenticated state, opaque
snapshot refs, and a human input channel. These do not require restoring raw CDP
or exporting cookies. Streaming alone does not establish exclusive ownership:
our model operations must remain paused while a human control lease exists.

Redaction is defense in depth against ordinary accidental echoes. It cannot prove
secrecy from an approved website that deliberately transforms, splits or draws a
credential it already received. The restricted projection excludes input values,
scripts, hidden content, subframes and raw attributes; credential variants are
filtered before text reaches the model. Session cookies and network bodies remain
unavailable. Do not describe these controls as universal DLP.

Snapshot action refs bind the node, document and surrounding form markup. They do
not freeze property-only changes to control values or externally associated form
controls. Consequential actions still require the user's authorization and a fresh
review; these refs are not a transaction payload approval mechanism. Named submit
buttons are omitted because native form submission would omit their name/value.
