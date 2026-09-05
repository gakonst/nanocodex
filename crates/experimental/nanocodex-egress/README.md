# nanocodex-egress

`nanocodex-egress` is Nanocodex's unpublished, experimental HTTP egress
transport. It owns the authenticated loopback proxy shared by application
protocol layers and host-side secret replacement.

The crate owns proxy authentication, TLS interception, bounded replayable
request bodies, aggregate memory and forwarding concurrency, setup deadlines,
streaming responses, SSRF/DNS-rebinding guards, and bounded lifecycle.
Application protocols remain outside the crate and compose through
`EgressLayer`; the Nanocodex binary implements its Tempo payment layer using
MPP's request middleware. The bundled layers provide static default-deny
request policy, request-header filtering, and host-owned secrets.

```rust,no_run
use nanocodex_egress::{
    EgressProxy, SecretEgress, SecretRef, SecretRule, StaticSecretResolver,
};
use nanocodex_egress::http::Method;

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let reference = SecretRef::new("memory", "service-token");
let resolver = StaticSecretResolver::new()
    .with_secret(reference.clone(), "host-only-value");
let rule = SecretRule::builder("service", reference, "https://service.example")
    .method(Method::POST)
    .path_prefix("/v1/execute")
    .replace_header("authorization", "nanocodex-secret-service")
    .child_environment("SERVICE_BASE_URL", "SERVICE_API_KEY")
    .build()?;
let secrets = SecretEgress::builder(resolver).rule(rule).build()?;
let proxy = EgressProxy::builder()
    .layer(secrets)
    .spawn()
    .await?;

let child_environment = proxy.environment();
assert!(!child_environment.is_empty());
assert!(child_environment.iter().all(|(_, value)| value != "host-only-value"));
assert!(proxy.ca_certificate_path().is_file());
proxy.shutdown().await?;
# Ok(())
# }
```

Layers run in builder order. Apply `environment()` only to tool child
processes, not to Nanocodex's control-plane process. It contains the proxy's
short-lived authentication capability, the ephemeral CA path, and each secret
rule's public base URL and placeholder; it never contains a resolved secret.

A secret rule binds one operation to an exact origin plus optional methods and
path-segment prefixes. Replacement can scan any number of named headers, query
values, the path, and a buffered body; injection can overwrite one header or
query parameter. `SecretFormat` supports raw, Bearer, Basic, and fixed-affix
wire values. Multiple rules may apply to one request, so independent
credentials compose in declaration order. Requests to a claimed origin fail
closed when method, path, or a required placeholder does not match. Remote
upstreams must use HTTPS; plaintext HTTP is accepted only for deliberate
loopback development services. The resolver runs only after policy accepts a
request, allowing application-defined secret stores, refresh, rotation, JSON
extraction, or short-lived token minting without putting those capabilities in
child code.

`SecretEgress` denies destinations not claimed by a rule by default. Set
`UnmatchedEgress::Allow` only when another layer or the origin should receive
unclaimed traffic. CONNECT tunnels are checked before an origin connection is
opened, and layer-provided child variables cannot override proxy transport
variables.

`EgressPolicy` is an independent exact-origin/method/path allowlist with
enforcing and observation-only warn modes. `HeaderAllowlist` strips all request
headers except configured exact names and prefixes, optionally within matching
request scopes. Put policy first, secret mutation next, and header filtering
after mutation when composing them.

The origin client denies loopback and the AWS/GCP metadata IPs by default,
including after DNS resolution. `allow_loopback_upstreams(true)` is an explicit
development escape hatch; metadata remains denied. Redirects are disabled, an
unrecognized CONNECT can never fall back to an opaque tunnel, and protocol
upgrades are rejected until they can run the same mutation pipeline. Ordinary
SSE and other HTTP response bodies stream without full buffering.

## Iron Proxy capability mapping

The embedded API deliberately uses typed Rust builders rather than Iron's YAML,
DNS server, or daemon control plane:

- Built in: default-deny URL policy and warn mode, header allowlisting,
  host-owned inject/replace secrets, header/query/path/body placements,
  required placeholders, common formatters, ordered transforms, exact request
  scoping, structured tracing, body/concurrency limits, SSRF guards, CONNECT,
  HTTPS MITM, and streamed HTTP/SSE responses.
- Supplied through `SecretResolver`: environment, file, AWS Secrets Manager,
  AWS SSM, 1Password, Vault, OAuth/GCP token minting, caching, refresh, and JSON
  extraction. The crate intentionally does not force those SDKs on embedders.
- Supplied as application `EgressLayer`s: HMAC, AWS SigV4, OAuth/GCP auth,
  annotation/body capture, external gRPC policy, LLM judge/circuit breaking,
  synthetic responses, and response transforms. The layer receives the owned
  replayable request and can wrap the rest of the ordered stack.
- Owned elsewhere: MCP tool policy stays in `nanocodex-tools`; VM networking
  and guest projection stay in `nanocodex-vm`; Tempo payment stays under
  `bin/`. DNS interception, TPROXY/nftables deployment, SOCKS5, PostgreSQL MITM,
  hot-reload management APIs, and a hosted control plane are daemon concerns,
  not an embedded SDK transport.

`EgressProxy::route()` exposes the same short-lived proxy capability and public
CA for a host child, VM, or container path. The repository's
[`secret-egress`](../../../examples/secret_egress.rs) example runs a real
Nanocodex Code Mode `Promise.all` curl fanout both on the host and in a retained
libkrun TSI VM; model traffic remains outside the tool egress route.
