//! VM and Docker Hands: preflight, launch, attachment and shutdown.
use std::time::Instant;

use nanocodex_managed::{ManagedClient, ManagedError};
use nanocodex_oai_tools::attachment::{Attachment, AttachmentMetadata, AttachmentTarget};
use tracing::Instrument as _;

use super::{Hand, HandNetwork, client_from_environment, service, vm_hand};

/// Serve a VM or Docker Hand: the Hand daemon and the managed tree share this path.
pub(crate) async fn serve_isolated_hand(command: Hand) -> Result<(), ManagedError> {
    let _observability = command
        .observability
        .install()
        .map_err(|error| ManagedError::Configuration(error.to_string()))?;
    tracing::info!(target: "nanocodex2", stage = "hand.preflight",
        machine.id = command.machine_id(),
        hand.backend = if command.docker.is_some() { "docker" } else { "vm" },
        vm.cpu.count = command.vm_cpus,
        vm.memory.limit_mib = command.vm_memory_mib,
        vm.root.kind = command.rootfs.as_ref().map_or("container", |root| if root.exists() { "existing" } else { "missing" }),
        "checking Hand backend support");
    if let Err(error) = vm_hand::VmHand::preflight(&command).await {
        tracing::error!(target: "nanocodex2", stage = "hand.preflight.failed", "Hand backend preflight failed");
        return Err(error);
    }
    let client = client_from_environment(None)?;
    serve_vm_hand(&client, command).await
}

async fn launch_vm_hand(command: &Hand) -> Result<vm_hand::VmHand, ManagedError> {
    let (root_kind, root_bytes) = match command.rootfs.as_ref().map(std::fs::metadata) {
        Some(Ok(metadata)) if metadata.is_file() => ("file", metadata.len()),
        Some(Ok(metadata)) if metadata.is_dir() => ("directory", 0),
        Some(Ok(_)) => ("other", 0),
        Some(Err(_)) => ("missing", 0),
        None => ("docker", 0),
    };
    let span = tracing::info_span!(
        target: "nanocodex2",
        "vm.launch",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        machine.id = command.machine_id(),
        vm.cpu.count = command.vm_cpus,
        vm.memory.limit_mib = command.vm_memory_mib,
        vm.root.kind = root_kind,
        vm.root.bytes = root_bytes,
        network.enabled = command.network.map_or(if command.docker.is_some() { command.docker_internet } else { !command.vm_no_network }, |network| network == HandNetwork::Internet),
        hand.backend = if command.docker.is_some() { "docker" } else { "libkrun" },
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let started = Instant::now();
    async {
        tracing::info!(
            target: "nanocodex2",
            stage = "vm.launch.starting",
            "starting Hand workspace"
        );
        let result = vm_hand::VmHand::start(command).await;
        span.record(
            "duration_ns",
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        match &result {
            Ok(_) => {
                span.record("status", "ready");
                span.record("otel.status_code", "OK");
                tracing::info!(
                    target: "nanocodex2",
                    stage = "vm.launch.ready",
                    "Hand guest is ready"
                );
            }
            Err(_) => {
                span.record("status", "failed");
                span.record("otel.status_code", "ERROR");
                tracing::error!(
                    target: "nanocodex2",
                    stage = "vm.launch.failed",
                    "Hand guest failed to start"
                );
            }
        }
        result
    }
    .instrument(span.clone())
    .await
}

async fn serve_vm_hand(client: &ManagedClient, command: Hand) -> Result<(), ManagedError> {
    let target = client.account_attachment_target()?;
    let mut hand = launch_vm_hand(&command).await?;
    drop(command);
    let connected = async {
        hand.start_desktop(&target).await?;
        connect_vm_hand(&hand, target).await
    }
    .await;
    let attachment = match connected {
        Ok(Some(attachment)) => attachment,
        Ok(None) => {
            shutdown_vm_hand(hand).await?;
            return Ok(());
        }
        Err(error) => {
            return match shutdown_vm_hand(hand).await {
                Ok(()) => Err(error),
                Err(shutdown) => Err(ManagedError::Configuration(format!(
                    "{error}; VM shutdown also failed: {shutdown}"
                ))),
            };
        }
    };
    tracing::info!(
        target: "nanocodex2",
        stage = "vm.hand.ready",
        "Hand is ready; press Ctrl-C to detach"
    );
    let closed = attachment.clone();
    let attachment_result = tokio::select! {
        signal = service::shutdown_signal() => {
            signal?;
            attachment.clone().detach().await
        }
        result = closed.closed() => result,
    };
    drop(attachment);
    drop(closed);
    let shutdown = shutdown_vm_hand(hand).await;
    match (attachment_result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(ManagedError::Configuration(error.to_string())),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(shutdown)) => Err(ManagedError::Configuration(format!(
            "{error}; VM shutdown also failed: {shutdown}"
        ))),
    }
}

async fn shutdown_vm_hand(hand: vm_hand::VmHand) -> Result<(), ManagedError> {
    let span = tracing::info_span!(
        target: "nanocodex2",
        "vm.shutdown",
        otel.kind = "internal",
        otel.status_code = tracing::field::Empty,
        status = tracing::field::Empty,
        duration_ns = tracing::field::Empty,
    );
    let started = Instant::now();
    async {
        tracing::info!(
            target: "nanocodex2",
            stage = "vm.shutdown.starting",
            "stopping Hand guest"
        );
        let result = hand.shutdown().await;
        span.record(
            "duration_ns",
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
        );
        if result.is_ok() {
            span.record("status", "completed");
            span.record("otel.status_code", "OK");
            tracing::info!(
                target: "nanocodex2",
                stage = "vm.shutdown.completed",
                "Hand guest stopped"
            );
        } else {
            span.record("status", "failed");
            span.record("otel.status_code", "ERROR");
            tracing::error!(
                target: "nanocodex2",
                stage = "vm.shutdown.failed",
                "Hand guest failed to stop cleanly"
            );
        }
        result
    }
    .instrument(span.clone())
    .await
}

async fn connect_vm_hand(
    hand: &vm_hand::VmHand,
    target: AttachmentTarget,
) -> Result<Option<Attachment>, ManagedError> {
    let connector = hand
        .tools()
        .attach(target)
        .metadata(AttachmentMetadata::machine(hand.machine().clone()));
    let connected = tokio::select! {
        signal = service::shutdown_signal() => {
            signal?;
            Ok(None)
        }
        connected = connector.connect() => connected
            .map(Some)
            .map_err(|error| ManagedError::Configuration(error.to_string())),
    };
    connected.map(|connected| connected.map(|(attachment, _events)| attachment))
}
