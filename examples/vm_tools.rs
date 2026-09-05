use std::{
    error::Error,
    fs, io,
    path::{Path, PathBuf},
};

use nanocodex::{
    Tool,
    tools::contract::{ToolContext, ToolInput, ToolOutput, ToolOutputBody, ToolOutputContent},
};
use nanocodex_vm::{
    VmWorkspace, VmWorkspaceBuilder, host::VmProcessConfig, tools::GuestRuntimeDisk,
};
use serde::Deserialize;
use serde_json::value::to_raw_value;

type AnyError = Box<dyn Error + Send + Sync>;

#[derive(Deserialize)]
struct CommandOutput {
    output: String,
    exit_code: Option<i32>,
    session_id: Option<i64>,
}

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
        .block_on(run_host(arguments))
}

#[allow(
    clippy::too_many_lines,
    reason = "the executable intentionally presents one linear end-to-end VM tool proof"
)]
async fn run_host(arguments: Vec<std::ffi::OsString>) -> Result<(), AnyError> {
    let root = arguments
        .first()
        .cloned()
        .map(PathBuf::from)
        .ok_or("usage: vm-tools ROOTFS [GUEST_RUNTIME_BINARY_OR_EXT4]")?;
    let executable = std::env::current_exe()?;
    let (private_root, mut workspace) = if root.is_file() {
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
    let runtime = prepared_runtime
        .as_ref()
        .map(|runtime| runtime.path().to_owned())
        .or(runtime_input);
    if let Some(runtime) = runtime {
        workspace = workspace.guest_runtime_disk(runtime);
    }
    workspace = workspace
        .vmm_argument("--vmm")
        .guest_workspace("/workspace")
        .cpus(2)
        .memory_mib(768)
        .offline();
    if let Some(loader_path) = std::env::var_os(if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    }) {
        workspace = workspace.firmware_directory(loader_path);
    }
    let workspace = workspace.launch().await?;
    let vm = workspace.tools();
    let agent_tools = workspace.tools_builder().build()?;
    let context = ToolContext::new("vm-proof", "session-1", "call-1", &[], 10_000);

    let execution = vm
        .exec_command_tool()
        .execute(
            function_input(&serde_json::json!({
                "cmd": "printf 'kernel='; uname -srm; printf 'pid1='; cat /proc/1/comm",
                "workdir": "/workspace",
                "login": false
            }))?,
            context,
        )
        .await?;
    let output = command_output(execution)?;
    println!("exec_command: {}", output.output.trim());
    if output.exit_code != Some(0) {
        return Err("exec_command did not exit successfully".into());
    }

    let execution = vm
        .exec_command_tool()
        .execute(
            function_input(&serde_json::json!({
                "cmd": "cat",
                "workdir": "/workspace",
                "login": false,
                "tty": true,
                "yield_time_ms": 250
            }))?,
            context,
        )
        .await?;
    let output = command_output(execution)?;
    let shell_session = output
        .session_id
        .ok_or("exec_command did not retain an interactive session")?;
    println!("exec_command session: {shell_session}");

    let execution = vm
        .write_stdin_tool()
        .execute(
            function_input(&serde_json::json!({
                "session_id": shell_session,
                "chars": "from-host\n",
                "yield_time_ms": 1_000
            }))?,
            context,
        )
        .await?;
    let mut output = command_output(execution)?;
    for _ in 0..3 {
        if output.output.contains("from-host") {
            break;
        }
        output = command_output(
            vm.write_stdin_tool()
                .execute(
                    function_input(&serde_json::json!({
                        "session_id": shell_session,
                        "yield_time_ms": 1_000
                    }))?,
                    context,
                )
                .await?,
        )?;
    }
    println!("write_stdin: {}", output.output.trim());
    if !output.output.contains("from-host") {
        return Err("write_stdin did not reach the retained guest process".into());
    }

    let proof_file = format!("vm-proof-{}.txt", std::process::id());
    let patch = format!(
        "*** Begin Patch\n*** Add File: {proof_file}\n+changed inside the guest\n*** End Patch"
    );
    let execution = vm
        .apply_patch_tool()
        .execute(ToolInput::Freeform(patch), context)
        .await?;
    println!("apply_patch: {}", text_output(execution)?.trim());

    let execution = vm
        .exec_command_tool()
        .execute(
            function_input(&serde_json::json!({
                "cmd": "printf iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII= | base64 -d > pixel.png",
                "workdir": "/workspace",
                "login": false
            }))?,
            context,
        )
        .await?;
    if command_output(execution)?.exit_code != Some(0) {
        return Err("failed to prepare guest image fixture".into());
    }

    let execution = vm
        .view_image_tool()
        .execute(
            function_input(&serde_json::json!({
                "path": "pixel.png",
                "detail": "original"
            }))?,
            context,
        )
        .await?;
    let ToolOutputBody::Content(image_items) = execution.output else {
        return Err("view_image did not return multimodal content".into());
    };
    let Some(ToolOutputContent::InputImage { image_url, detail }) = image_items.into_iter().next()
    else {
        return Err("view_image did not return an image".into());
    };
    println!(
        "view_image: detail={detail:?}, data_url_bytes={}",
        image_url.len()
    );
    println!("all VM-owned tools executed through one retained libkrun VM");
    drop(agent_tools);
    drop(vm);
    workspace.shutdown().await?;
    drop(private_root);
    Ok(())
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

fn function_input(value: &serde_json::Value) -> Result<ToolInput, serde_json::Error> {
    to_raw_value(value).map(ToolInput::Function)
}

fn command_output(execution: ToolOutput) -> Result<CommandOutput, AnyError> {
    let text = text_output(execution)?;
    serde_json::from_str(&text).map_err(Into::into)
}

fn text_output(execution: ToolOutput) -> Result<String, AnyError> {
    if !execution.success {
        return Err("tool execution reported failure".into());
    }
    match execution.output {
        ToolOutputBody::Text(text) => Ok(text),
        ToolOutputBody::Content(_) => Err("expected text tool output".into()),
    }
}
