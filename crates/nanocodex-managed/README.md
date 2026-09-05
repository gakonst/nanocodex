# nanocodex-managed

Native account-managed lifecycle backend for Nanocodex. The crate owns the
authenticated managed HTTP service, resumable durable event stream, and
optional reverse attachment of a caller-owned `Tools` recipe. The cloud owns
model execution and retained history; this crate never reads provider
credentials or application environment variables.
