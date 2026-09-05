# Nanocodex OpenAI API contracts

Dependency-light prompt, response-item, and tool contracts shared by
Nanocodex process companions.

This is the crate documentation produced when the default `client` feature is
disabled. It contains no authentication, managed session, Tower service,
network transport, telemetry, or pricing implementation. Normal applications
should use the default features and the complete client API documented in the
package README.

[`Prompt`], [`Thinking`], [`ReasoningMode`], and [`ImageDetail`] define shared
input policy. [`responses`] contains provider response events, items, content,
and tool definitions. [`tools`] contains model-visible tool inputs, outputs,
contexts, and the lossless process-boundary representation used by
`nanocodex-tools`.

The contract-only surface exists so a static companion such as
`nanocodex-vm-guest` can reuse the exact same types without linking an unused
OpenAI client. It is not an alternate provider or transport implementation.
