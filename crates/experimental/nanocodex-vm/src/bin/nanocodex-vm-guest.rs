#[cfg(target_os = "linux")]
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    net::{Ipv4Addr, Ipv6Addr},
    os::unix::process::CommandExt as _,
    path::PathBuf,
    process::Command,
};

#[cfg(target_os = "linux")]
use futures_util::TryStreamExt as _;
#[cfg(target_os = "linux")]
use landlock::{
    AccessNet, CompatLevel, Compatible, NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus,
};
#[cfg(target_os = "linux")]
use nix::libc;
#[cfg(target_os = "linux")]
use rtnetlink::{Handle, RouteMessageBuilder, new_connection, packet_route::route::RouteMessage};
#[cfg(target_os = "linux")]
use rustix::thread::{
    CapabilitySet, capabilities, remove_capability_from_bounding_set, set_capabilities,
};
#[cfg(target_os = "linux")]
use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch, apply_filter,
};

#[cfg(target_os = "linux")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    if first.as_deref() == Some(OsStr::new("--host-capture-only")) {
        let port = capture_port(&mut arguments, "--host-capture-only")?;
        let program = arguments
            .next()
            .ok_or_else(|| invalid_input("--host-capture-only requires a program"))?;
        return run_host_capture_only(port, program, arguments.collect());
    }
    if first.as_deref() == Some(OsStr::new("--capture-only")) {
        let port = capture_port(&mut arguments, "--capture-only")?;
        let program = arguments
            .next()
            .ok_or_else(|| invalid_input("--capture-only requires a program"))?;
        return run_capture_only(port, program, arguments.collect()).await;
    }
    if first.as_deref() == Some(OsStr::new("--overlay-root")) {
        let workspace = arguments.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "--overlay-root requires a guest workspace",
            )
        })?;
        let resolver = arguments.next().unwrap_or_default();
        if arguments.next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--overlay-root accepts only WORKSPACE and optional RESOLVER",
            )
            .into());
        }
        let resolver = resolver.to_string_lossy();
        return nanocodex_vm::tools::serve_overlay_guest(
            PathBuf::from(workspace),
            (!resolver.is_empty()).then_some(resolver.as_ref()),
        )
        .await
        .map_err(Into::into);
    }

    let workspace = first.map_or_else(|| PathBuf::from("/workspace"), PathBuf::from);
    if arguments.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "guest runtime accepts only one workspace argument",
        )
        .into());
    }
    nanocodex_vm::tools::serve_guest(workspace)
        .await
        .map_err(Into::into)
}

#[cfg(target_os = "linux")]
fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(target_os = "linux")]
fn capture_port(
    arguments: &mut impl Iterator<Item = OsString>,
    mode: &str,
) -> Result<u16, io::Error> {
    arguments
        .next()
        .ok_or_else(|| invalid_input(format!("{mode} requires a TCP port")))?
        .to_string_lossy()
        .parse::<u16>()
        .map_err(|error| invalid_input(format!("invalid capture TCP port: {error}")))
}

#[cfg(target_os = "linux")]
async fn run_capture_only(
    _port: u16,
    program: OsString,
    arguments: Vec<OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    remove_default_routes().await?;
    drop_network_capabilities()?;
    restrict_socket_families()?;
    Err(Command::new(program).args(arguments).exec().into())
}

#[cfg(target_os = "linux")]
fn drop_network_capabilities() -> Result<(), Box<dyn std::error::Error>> {
    let network = CapabilitySet::NET_ADMIN | CapabilitySet::NET_RAW;
    for capability in [CapabilitySet::NET_ADMIN, CapabilitySet::NET_RAW] {
        remove_capability_from_bounding_set(capability)?;
    }
    let mut sets = capabilities(None)?;
    sets.effective.remove(network);
    sets.permitted.remove(network);
    sets.inheritable.remove(network);
    set_capabilities(None, sets)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_host_capture_only(
    port: u16,
    program: OsString,
    arguments: Vec<OsString>,
) -> Result<(), Box<dyn std::error::Error>> {
    restrict_tcp_to_port(port)?;
    restrict_socket_families()?;
    Err(Command::new(program).args(arguments).exec().into())
}

#[cfg(target_os = "linux")]
async fn remove_default_routes() -> Result<(), Box<dyn std::error::Error>> {
    let (connection, handle, _) = new_connection()?;
    let connection = tokio::spawn(connection);
    delete_default_routes(&handle, RouteMessageBuilder::<Ipv4Addr>::new().build()).await?;
    delete_default_routes(&handle, RouteMessageBuilder::<Ipv6Addr>::new().build()).await?;
    connection.abort();
    Ok(())
}

#[cfg(target_os = "linux")]
async fn delete_default_routes(
    handle: &Handle,
    request: RouteMessage,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut routes = handle.route().get(request).execute();
    let mut defaults = Vec::new();
    while let Some(route) = routes.try_next().await? {
        if route.header.destination_prefix_length == 0 {
            defaults.push(route);
        }
    }
    drop(routes);
    for route in defaults {
        handle.route().del(route).execute().await?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn restrict_tcp_to_port(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let status = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessNet::ConnectTcp)?
        .handle_access(AccessNet::BindTcp)?
        .create()?
        .add_rule(NetPort::new(port, AccessNet::ConnectTcp))?
        .restrict_self()?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(io::Error::other(format!(
            "capture-only Landlock rules were not fully enforced: {status:?}"
        ))
        .into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn restrict_socket_families() -> Result<(), Box<dyn std::error::Error>> {
    let deny_non_inet_or_unix = SeccompRule::new(vec![
        SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            libc::AF_INET as u64,
        )?,
        SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            libc::AF_UNIX as u64,
        )?,
    ])?;
    let mut deny_non_stream = vec![SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Eq,
        libc::AF_INET as u64,
    )?];
    for allowed_type in [
        libc::SOCK_STREAM,
        libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
        libc::SOCK_STREAM | libc::SOCK_NONBLOCK,
        libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
    ] {
        deny_non_stream.push(SeccompCondition::new(
            1,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::Ne,
            allowed_type as u64,
        )?);
    }
    let deny_non_unix_socketpair = SeccompRule::new(vec![SeccompCondition::new(
        0,
        SeccompCmpArgLen::Dword,
        SeccompCmpOp::Ne,
        libc::AF_UNIX as u64,
    )?])?;
    let mut rules = BTreeMap::new();
    rules.insert(
        libc::SYS_socket,
        vec![deny_non_inet_or_unix, SeccompRule::new(deny_non_stream)?],
    );
    rules.insert(libc::SYS_socketpair, vec![deny_non_unix_socketpair]);
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::EPERM as u32),
        if cfg!(target_arch = "x86_64") {
            TargetArch::x86_64
        } else if cfg!(target_arch = "aarch64") {
            TargetArch::aarch64
        } else {
            return Err(io::Error::other("unsupported capture-only architecture").into());
        },
    )?;
    let program: BpfProgram = filter.try_into()?;
    apply_filter(&program)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("nanocodex-vm-guest must be built for a Linux guest target");
}
