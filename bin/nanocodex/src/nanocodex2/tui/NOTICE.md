# Tact-derived terminal interface

Portions of this directory are derived from
[clabby/tact](https://github.com/clabby/tact) revision
`e20b1584642339546bb2310aad6968edeec66a53`, modified for Nanocodex2.

Those portions are distributed under the Apache License 2.0. The upstream
project did not contain a separate `NOTICE` file at the pinned revision. The
repository's `LICENSE-APACHE` contains the applicable license text.

Nanocodex2 does not start Tact's local agent, memory, or subagent runtimes. The
memory browser is intentionally omitted; hosted tools and the existing
Nanocodex workspace attachment remain the owners of those capabilities. The
copied presentation may use Nanocodex's own subagent protocol types to render
hosted events, but it does not create or supervise a subagent runtime.
