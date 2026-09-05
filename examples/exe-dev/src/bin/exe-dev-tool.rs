//! Runs Nanocodex on the host with one caller-owned exe.dev VM as its tool.

use std::{env, path::PathBuf};

use chrono::Utc;
use eyre::{Result, WrapErr, bail};
use nanocodex::{
    Nanocodex, OpenAi, Thinking,
    agent::ExecutionEnvironment,
    oai::auth::{OpenAiAuth, load_chatgpt_auth},
};
use nanocodex_exe_dev::sandbox::{ExeDevSandbox, SandboxCleanup, SandboxConfig};
use uuid::Uuid;

const DEFAULT_PROMPT: &str = "Create the remote sandbox, use it to report `uname -srm`, write the exact text `created by external nanocodex` to `~/nanocodex-proof.txt`, verify the file, and report the sandbox name and private preview URL.";

#[tokio::main]
async fn main() -> Result<()> {
    let name = sandbox_name()?;
    let cleanup = cleanup_policy()?;
    let mut config = SandboxConfig::new(name)?;
    if let Some(binary) = env::var_os("NANOCODEX_EXE_SSH") {
        config.ssh_binary = PathBuf::from(binary);
    }
    let sandbox = ExeDevSandbox::new(config)?;
    let result = run_agent(
        &sandbox,
        env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_PROMPT.to_owned()),
    )
    .await;
    let cleanup_result = sandbox.cleanup(cleanup).await;

    match cleanup_result {
        Ok(Some(operation)) if operation.exit_code == Some(0) => {
            eprintln!(
                "deleted caller-owned sandbox {}; closed {} SSH control masters",
                sandbox.name(),
                operation.control_masters_closed
            );
        }
        Ok(Some(operation)) => {
            bail!(
                "failed to delete sandbox {}: exit={:?}, stderr={}",
                sandbox.name(),
                operation.exit_code,
                operation.stderr.trim()
            );
        }
        Ok(None) if cleanup == SandboxCleanup::Retain => {
            eprintln!(
                "retained caller-owned sandbox {} at https://{}.exe.xyz",
                sandbox.name(),
                sandbox.name()
            );
        }
        Ok(None) => {}
        Err(error) => return Err(error).wrap_err("sandbox cleanup failed"),
    }

    println!("{}", result?);
    Ok(())
}

fn sandbox_name() -> Result<String> {
    select_sandbox_name(
        env::var("NANOCODEX_EXE_SANDBOX").ok(),
        env::var("CODEX_THREAD_ID").ok(),
    )
}

fn select_sandbox_name(explicit: Option<String>, thread_id: Option<String>) -> Result<String> {
    if let Some(explicit) = explicit {
        return Ok(explicit);
    }
    let thread_id = thread_id.ok_or_else(|| {
        eyre::eyre!("set CODEX_THREAD_ID or the explicit NANOCODEX_EXE_SANDBOX override")
    })?;
    let thread_id = Uuid::parse_str(&thread_id)
        .wrap_err("CODEX_THREAD_ID must be a UUID when used for an exe.dev sandbox name")?;
    Ok(format!("nanocodex-{}", thread_id.simple()))
}

async fn run_agent(sandbox: &ExeDevSandbox, prompt: String) -> Result<String> {
    let auth = model_auth()?;
    let openai = OpenAi::new(auth).wrap_err("failed to configure OpenAI")?;
    let tools = sandbox.tools()?;
    let (agent, events) = Nanocodex::builder(openai)
        .instructions(
            "You operate exactly one caller-owned remote exe.dev sandbox. Call sandbox_create before sandbox_exec. Execute all shell work through sandbox_exec; host workspace tools are unavailable. The sandbox name and deletion policy are fixed by the caller. Never claim a command succeeded unless its structured result reports exit_code 0.",
        )
        .thinking(Thinking::Low)
        .tools(tools)
        .execution_environment(ExecutionEnvironment::new(
            Utc::now().date_naive().to_string(),
            "Etc/UTC",
        ))
        .build()?;
    drop(events);

    let result = agent.prompt(prompt).await?.result().await;
    let shutdown = agent.shutdown().await;
    shutdown.wrap_err("failed to shut down the external Nanocodex session")?;
    result
        .map(|result| result.into_final_message())
        .wrap_err("external Nanocodex turn failed")
}

fn model_auth() -> Result<OpenAiAuth> {
    if let Ok(api_key) = env::var("OPENAI_API_KEY")
        && !api_key.trim().is_empty()
    {
        return Ok(OpenAiAuth::api_key(api_key));
    }
    let auth_file = env::var_os("NANOCODEX_AUTH_FILE")
        .map(PathBuf::from)
        .or_else(|| env::var_os("CODEX_HOME").map(|root| PathBuf::from(root).join("auth.json")))
        .or_else(|| env::var_os("HOME").map(|root| PathBuf::from(root).join(".codex/auth.json")))
        .ok_or_else(|| eyre::eyre!("set OPENAI_API_KEY or NANOCODEX_AUTH_FILE"))?;
    load_chatgpt_auth(&auth_file).wrap_err_with(|| {
        format!(
            "failed to load ChatGPT authorization from {}",
            auth_file.display()
        )
    })
}

fn cleanup_policy() -> Result<SandboxCleanup> {
    match env::var("NANOCODEX_EXE_CLEANUP")
        .unwrap_or_else(|_| "retain".to_owned())
        .as_str()
    {
        "retain" => Ok(SandboxCleanup::Retain),
        "delete" => Ok(SandboxCleanup::Delete),
        value => bail!("NANOCODEX_EXE_CLEANUP must be `retain` or `delete`, got `{value}`"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_name_overrides_the_host_thread() {
        assert_eq!(
            select_sandbox_name(
                Some("named-by-caller".to_owned()),
                Some("not-a-uuid".to_owned())
            )
            .unwrap(),
            "named-by-caller"
        );
    }

    #[test]
    fn codex_thread_id_becomes_a_stable_valid_vm_name() {
        assert_eq!(
            select_sandbox_name(
                None,
                Some("019fc99b-15d2-7470-9ce5-d14ce353d2f8".to_owned())
            )
            .unwrap(),
            "nanocodex-019fc99b15d274709ce5d14ce353d2f8"
        );
    }

    #[test]
    fn missing_or_invalid_thread_identity_fails_closed() {
        assert!(select_sandbox_name(None, None).is_err());
        assert!(select_sandbox_name(None, Some("process-42".to_owned())).is_err());
    }
}
