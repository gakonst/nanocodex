use std::error::Error;

use nanocodex_egress::{
    EgressProxy, SecretEgress, SecretRef, SecretRule, StaticSecretResolver, http::Method,
};

fn main() -> Result<(), Box<dyn Error>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<(), Box<dyn Error>> {
    let reference = SecretRef::new("memory", "service-token");
    let resolver =
        StaticSecretResolver::new().with_secret(reference.clone(), "host-only-example-value");
    let rule = SecretRule::builder("service", reference, "https://service.example")
        .method(Method::POST)
        .path_prefix("/v1/execute")
        .replace_header("authorization", "nanocodex-secret-service")
        .child_environment("SERVICE_BASE_URL", "SERVICE_API_KEY")
        .build()?;
    let secrets = SecretEgress::builder(resolver).rule(rule).build()?;
    let proxy = EgressProxy::builder().layer(secrets).spawn().await?;

    assert!(
        proxy
            .environment()
            .iter()
            .all(|(_, value)| value != "host-only-example-value")
    );
    println!(
        "composed host-secret egress with {} child variables",
        proxy.environment().len()
    );
    proxy.shutdown().await?;
    Ok(())
}
