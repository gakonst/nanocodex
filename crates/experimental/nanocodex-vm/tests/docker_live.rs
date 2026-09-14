//! Run with NANOCODEX_DOCKER_TEST_IMAGE=nanocodex-hand:local cargo test
//! -p nanocodex-vm --test docker_live -- --ignored --nocapture.
#![cfg(any(
    all(target_os = "linux", not(target_env = "musl")),
    all(target_os = "macos", target_arch = "aarch64")
))]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex_tools::{
    ToolContext, ToolInput,
    contract::{ToolOutputBody, ToolOutputContent},
    runtime::ToolRuntime,
};
use nanocodex_vm::{
    docker::{DockerWorkspace, DockerWorkspaceError},
    tools::{VmCommand, VmToolSessionError},
};
use serde_json::{json, value::to_raw_value};
use std::{
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Volume(String);
impl Volume {
    fn new() -> Self {
        Self(format!(
            "nanocodex-docker-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    async fn containers(&self) -> String {
        docker(&[
            "container",
            "ls",
            "--all",
            "--filter",
            &format!("volume={}", self.0),
            "--format",
            "{{.ID}}",
        ])
        .await
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        // Unique test-only volumes; never enumerate or prune unrelated resources.
        let output = std::process::Command::new("docker")
            .args([
                "container",
                "ls",
                "--all",
                "--filter",
                &format!("volume={}", self.0),
                "--format",
                "{{.ID}}",
            ])
            .output()
            .unwrap();
        for id in String::from_utf8_lossy(&output.stdout).lines() {
            let _ = std::process::Command::new("docker")
                .args(["container", "rm", "--force", id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        let _ = std::process::Command::new("docker")
            .args(["volume", "rm", &self.0])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
fn image() -> String {
    std::env::var("NANOCODEX_DOCKER_TEST_IMAGE")
        .expect("set NANOCODEX_DOCKER_TEST_IMAGE to the built Hand image")
}
async fn docker(args: &[&str]) -> String {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("docker")
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
async fn tool(runtime: &ToolRuntime, name: &str, input: ToolInput) -> String {
    let output = runtime
        .execute_tool(
            name,
            input,
            ToolContext::new("test", "docker-live", "call", &[], 4000),
        )
        .await
        .unwrap();
    assert!(output.success, "{:?}", output.output);
    match output.output {
        ToolOutputBody::Text(text) => text,
        other => panic!("unexpected output: {other:?}"),
    }
}
fn function(value: serde_json::Value) -> ToolInput {
    ToolInput::Function(to_raw_value(&value).unwrap())
}

#[tokio::test]
#[ignore = "requires a Linux Docker daemon and the built Hand image; no KVM needed"]
async fn tools_isolation_exclusive_ownership_and_persistence() {
    let volume = Volume::new();
    let workspace = DockerWorkspace::builder(image(), &volume.0)
        .launch()
        .await
        .unwrap();
    let control = workspace.control();
    let output = control
        .command(VmCommand::new("/bin/sh").arg("-ec").arg(concat!(
            "test \"$(id -u)\" = 1000; ",
            "test ! -e /dev/kvm; test ! -e /var/run/docker.sock; ",
            "test -z \"${NANOCODEX_DOCKER_HOST_SENTINEL-}\"; ",
            "grep -q 'CapEff:.*0000000000000000' /proc/self/status; ",
            "grep -q 'NoNewPrivs:.*1' /proc/self/status; ",
            "! touch /etc/nanocodex-test; ",
            "test ! -e /sys/class/net/eth0; ",
            "test \"$TMPDIR\" = /app/.tmp; ",
            "temporary=$(mktemp); printf '#!/bin/sh\\nexit 0\\n' > \"$temporary\"; ",
            "chmod 700 \"$temporary\"; \"$temporary\"; rm \"$temporary\"; ",
            "printf isolation-ok"
        )))
        .await
        .unwrap();
    assert_eq!(
        output.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"isolation-ok");
    let id = volume.containers().await;
    let inspect: serde_json::Value =
        serde_json::from_str(&docker(&["inspect", id.trim()]).await).unwrap();
    assert_eq!(inspect[0]["HostConfig"]["Memory"], 1024 * 1024 * 1024);
    assert_eq!(inspect[0]["HostConfig"]["NanoCpus"], 2_000_000_000u64);
    assert_eq!(inspect[0]["HostConfig"]["NetworkMode"], "none");
    assert_eq!(inspect[0]["HostConfig"]["ReadonlyRootfs"], true);

    let tools = workspace
        .tools_builder()
        .web_search(false)
        .image_generation(false)
        .build()
        .unwrap();
    let runtime = ToolRuntime::new_with_tools("/app", None, None, &tools);
    let patched = tool(
        &runtime,
        "apply_patch",
        ToolInput::Freeform(
            "*** Begin Patch\n*** Add File: /app/persistent.txt\n+retained\n*** End Patch".into(),
        ),
    )
    .await;
    assert!(patched.contains("persistent.txt"), "{patched}");
    let first = tool(&runtime, "exec_command", function(json!({"cmd":"read line; printf 'received:%s' \"$line\"", "tty":true,"yield_time_ms":100,"login":false}))).await;
    let sid: u64 = first
        .lines()
        .find_map(|line| line.strip_prefix("Process running with session ID "))
        .unwrap()
        .parse()
        .unwrap();
    let resumed = tool(
        &runtime,
        "write_stdin",
        function(json!({"session_id":sid,"chars":"hello\n","yield_time_ms":1000})),
    )
    .await;
    assert!(resumed.contains("received:hello"), "{resumed}");

    assert!(
        DockerWorkspace::builder(image(), &volume.0)
            .launch()
            .await
            .is_err()
    );
    assert_eq!(
        control.read_file("/app/persistent.txt").await.unwrap(),
        b"retained\n"
    );
    assert!(matches!(
        workspace.shutdown().await,
        Err(DockerWorkspaceError::Session(
            VmToolSessionError::ActiveCapabilities(_)
        ))
    ));
    drop(runtime);
    drop(tools);
    drop(control);
    workspace.shutdown().await.unwrap();
    assert!(volume.containers().await.trim().is_empty());
    let next = DockerWorkspace::builder(image(), &volume.0)
        .launch()
        .await
        .unwrap();
    assert_eq!(
        next.control()
            .read_file("/app/persistent.txt")
            .await
            .unwrap(),
        b"retained\n"
    );
    let (first, second) = tokio::join!(next.shutdown(), next.shutdown());
    first.unwrap();
    assert!(matches!(
        second,
        Err(DockerWorkspaceError::Session(VmToolSessionError::Closed))
    ));
}

#[tokio::test]
#[ignore = "requires a Linux Docker daemon and the built Hand image; no KVM needed"]
async fn startup_failure_cleans_up_and_explicit_internet_works() {
    let volume = Volume::new();
    assert!(
        DockerWorkspace::builder(image(), &volume.0)
            .runtime("nanocodex-missing-runtime")
            .launch()
            .await
            .is_err()
    );
    assert!(volume.containers().await.trim().is_empty());
    let workspace = DockerWorkspace::builder(image(), &volume.0)
        .internet()
        .launch()
        .await
        .unwrap();
    let output = workspace
        .control()
        .command(
            VmCommand::new("/bin/sh")
                .arg("-ec")
                .arg("test -e /sys/class/net/eth0"),
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    workspace.shutdown().await.unwrap();
    assert!(volume.containers().await.trim().is_empty());
}

#[tokio::test]
#[ignore = "requires a Linux Docker daemon and the built Hand image; no KVM needed"]
async fn desktop_and_last_capability_cleanup() {
    let volume = Volume::new();
    let workspace = DockerWorkspace::builder(image(), &volume.0)
        .launch()
        .await
        .unwrap();
    let control = workspace.control();
    let runner = workspace.control();
    let task = tokio::spawn(async move {
        runner
            .command(
                VmCommand::new("/usr/local/bin/nanocodex-vm-guest")
                    .arg("--desktop")
                    .arg("/app")
                    .arg("/run/nanocodex-hand-desktop")
                    .timeout(Duration::from_secs(60)),
            )
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    while control
        .read_file("/run/nanocodex-hand-desktop/ready")
        .await
        .is_err()
    {
        assert!(
            !task.is_finished(),
            "desktop exited before readiness: {:?}",
            task.await
        );
        assert!(Instant::now() < deadline, "desktop readiness timed out");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let output = control
        .command(
            VmCommand::new("/usr/local/bin/nanocodex-vm-guest")
                .arg("--desktop-request")
                .arg(r#"{"action":"observe"}"#)
                .arg("/run/nanocodex-hand-desktop"),
        )
        .await
        .unwrap();
    assert_eq!(
        output.exit_code,
        0,
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let frame: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(frame["status"], "ok");
    let jpeg = STANDARD.decode(frame["jpeg"].as_str().unwrap()).unwrap();
    assert!(jpeg.starts_with(&[0xff, 0xd8, 0xff]));
    control
        .write_file("/app/screen.jpg", jpeg, 0o600)
        .await
        .unwrap();
    {
        let tools = workspace
            .tools_builder()
            .web_search(false)
            .image_generation(false)
            .build()
            .unwrap();
        let runtime = ToolRuntime::new_with_tools("/app", None, None, &tools);
        let output = runtime
            .execute_tool(
                "view_image",
                function(json!({"path":"/app/screen.jpg"})),
                ToolContext::new("test", "docker-live", "image", &[], 4000),
            )
            .await
            .unwrap();
        assert!(output.success, "{:?}", output.output);
        assert!(
            matches!(output.output, ToolOutputBody::Content(items) if items.iter().any(|item| matches!(item, ToolOutputContent::InputImage { .. })))
        );
    }

    let output = control
        .command(
            VmCommand::new("/usr/local/bin/nanocodex-vm-guest")
                .arg("--desktop-request")
                .arg(r#"{"action":"shutdown"}"#)
                .arg("/run/nanocodex-hand-desktop"),
        )
        .await
        .unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(task.await.unwrap().unwrap().exit_code, 0);
    drop(workspace);
    // The final control capability still owns the running container.
    assert_eq!(
        control
            .command(VmCommand::new("/bin/true"))
            .await
            .unwrap()
            .exit_code,
        0
    );
    drop(control);
    let deadline = Instant::now() + Duration::from_secs(35);
    while !volume.containers().await.trim().is_empty() {
        assert!(
            Instant::now() < deadline,
            "last capability did not release the container"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
