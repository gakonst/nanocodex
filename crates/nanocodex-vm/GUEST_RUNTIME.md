# Nanocodex VM guest runtime

Static Linux companion for the retained Nanocodex VM workspace.

This is the crate documentation produced with `guest-runtime` and without the
default `host` feature. [`tools::serve_guest`] keeps one canonical
`nanocodex-tools` workspace runtime alive and serves `exec_command`,
`write_stdin`, `apply_patch`, and `view_image` over the guest console.

The artifact deliberately excludes libkrun host control, OCI/image
preparation, OpenAI transport, Code Mode/QuickJS, MCP, and HTTP clients. It is
an implementation companion built from the same revision as its host, not a
second public execution model or a versioned remote service.

The complete lockstep host/guest wire contract is documented in the package
README. Applications should use the default `host` feature and its typed
workspace API rather than calling [`tools::serve_guest`] directly.
