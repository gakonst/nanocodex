use std::{
    error::Error,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

use nanocodex::{Nanocodex, OpenAi, Tools};
use nanocodex_egress::{EgressProxy, SecretEgress, SecretRef, SecretRule, StaticSecretResolver};
use nanocodex_vm::{
    VmWorkspace, VmWorkspaceBuilder,
    host::{EgressFile, EgressLease, GUEST_EGRESS_ROOT, VmProcessConfig},
    tools::GuestRuntimeDisk,
};

type AnyError = Box<dyn Error + Send + Sync>;

const PLACEHOLDER: &str = "nanocodex-public-demo-token";
const GUEST_CA: &str = "/tmp/nanocodex/egress/secret-egress-ca.pem";

fn main() -> Result<(), AnyError> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.first().is_some_and(|value| value == "--vmm") {
        let config = arguments
            .get(1)
            .cloned()
            .map(PathBuf::from)
            .ok_or("VMM mode requires a private configuration path")?;
        VmProcessConfig::read(config)?.run()?;
        return Ok(());
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(arguments))
}

async fn run(arguments: Vec<OsString>) -> Result<(), AnyError> {
    let _ = dotenvy::dotenv();
    let mode = arguments
        .first()
        .and_then(|value| value.to_str())
        .ok_or("usage: secret-egress host | secret-egress vm ROOTFS [GUEST_RUNTIME]")?;
    let upstream = std::env::var("NANOCODEX_SECRET_UPSTREAM")?;
    let allow_loopback = reqwest::Url::parse(&upstream)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    let secret = std::env::var("NANOCODEX_SECRET_VALUE")?;
    let source = SecretRef::new("example-memory", "demo-service");
    let rule = SecretRule::builder("demo-service", source.clone(), upstream)
        .method(nanocodex_egress::http::Method::GET)
        .replace_header("authorization", PLACEHOLDER)
        .child_environment("DEMO_SERVICE_BASE_URL", "DEMO_SERVICE_TOKEN")
        .build()?;
    let secrets = SecretEgress::builder(StaticSecretResolver::new().with_secret(source, secret))
        .rule(rule)
        .build()?;
    let proxy = EgressProxy::builder()
        .allow_loopback_upstreams(allow_loopback)
        .layer(secrets)
        .spawn()
        .await?;

    match mode {
        "host" if arguments.len() == 1 => {
            let tools = Tools::builder()
                .process_environment(proxy.environment())
                .build()?;
            run_agent(tools).await?;
        }
        "vm" => run_in_vm(&arguments[1..], &proxy).await?,
        _ => {
            return Err(
                "usage: secret-egress host | secret-egress vm ROOTFS [GUEST_RUNTIME]".into(),
            );
        }
    }

    proxy.shutdown().await?;
    Ok(())
}

async fn run_agent(tools: Tools) -> Result<(), AnyError> {
    let openai = OpenAi::new(std::env::var("OPENAI_API_KEY")?)?;
    let requests = std::env::var("NANOCODEX_SECRET_STRESS_REQUESTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8);
    let prompt = format!(
        "Use one Code Mode cell and Promise.all to make exactly {requests} parallel calls to \
         exec_command. Each call must run curl -fsS with `Authorization: Bearer \
         $DEMO_SERVICE_TOKEN` against `$DEMO_SERVICE_BASE_URL`. Verify every command exits zero, \
         then report a concise aggregate result without printing environment variables."
    );
    let (agent, _) = Nanocodex::builder(openai)
        .instructions(
            "Exercise the configured service only through workspace commands. Never inspect or \
             print proxy configuration; the service token is a public placeholder.",
        )
        .tools(tools)
        .build()?;
    let result = agent.prompt(prompt).await?.result().await?;
    println!("{}", result.final_message());
    Ok(())
}

async fn run_in_vm(arguments: &[OsString], proxy: &EgressProxy) -> Result<(), AnyError> {
    let root = arguments
        .first()
        .cloned()
        .map(PathBuf::from)
        .ok_or("VM mode requires ROOTFS")?;
    let executable = std::env::current_exe()?;
    let (private_root, mut builder) = if root.is_file() {
        let directory = tempfile::tempdir()?;
        let private = directory.path().join("rootfs.ext4");
        let builder = VmWorkspaceBuilder::private_from(&root, &private, &executable)?;
        (Some(directory), builder)
    } else {
        (None, VmWorkspace::builder(&root, &executable))
    };
    let runtime_input = arguments.get(1).cloned().map(PathBuf::from);
    let prepared_runtime = match runtime_input.as_deref() {
        Some(runtime) if is_elf(runtime)? => Some(GuestRuntimeDisk::prepare(runtime, ".cache/vm")?),
        Some(_) | None => None,
    };
    if let Some(runtime) = prepared_runtime
        .as_ref()
        .map(|runtime| runtime.path().to_owned())
        .or(runtime_input)
    {
        builder = builder.guest_runtime_disk(runtime);
    }

    let route = proxy.route();
    let mut lease = EgressLease::internet();
    lease.insert_file(EgressFile::new(GUEST_CA, route.ca_certificate_pem(), 0o644))?;
    for (name, value) in route.environment(GUEST_CA) {
        lease.insert_environment(os_string(name)?, os_string(value)?)?;
    }
    debug_assert!(Path::new(GUEST_CA).starts_with(GUEST_EGRESS_ROOT));

    builder = builder
        .vmm_argument("--vmm")
        .guest_workspace("/workspace")
        .cpus(2)
        .memory_mib(768)
        .egress(lease);
    if let Some(loader_path) = std::env::var_os(if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    }) {
        builder = builder.firmware_directory(loader_path);
    }
    let workspace = builder.launch().await?;
    run_agent(workspace.tools_builder().build()?).await?;
    workspace.shutdown().await?;
    drop(private_root);
    Ok(())
}

fn os_string(value: OsString) -> Result<String, AnyError> {
    value
        .into_string()
        .map_err(|_| "egress environment is not UTF-8".into())
}

fn is_elf(path: &Path) -> io::Result<bool> {
    let mut file = fs::File::open(path)?;
    let mut magic = [0_u8; 4];
    match io::Read::read_exact(&mut file, &mut magic) {
        Ok(()) => Ok(magic == *b"\x7fELF"),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error),
    }
}
