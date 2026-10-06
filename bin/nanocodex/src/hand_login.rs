//! Best-effort macOS Hand enrollment after a successfully saved account login.

pub(crate) async fn connect_after_login(receipt: &nanocodex_cli_auth::LoginReceipt) {
    #[cfg(target_os = "macos")]
    {
        if receipt.skip_hand {
            return;
        }
        if connect(receipt).await.is_err() {
            eprintln!(
                "Account sign-in succeeded. Hand connection is pending; run `nanocodex hand connect` to repair it."
            );
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = receipt;
}

#[cfg(target_os = "macos")]
async fn connect(receipt: &nanocodex_cli_auth::LoginReceipt) -> std::io::Result<()> {
    use std::{io, process::Stdio, time::Duration};
    use tokio::process::Command;

    // Resolve the installed executable beside this client, never through PATH.
    let current = std::env::current_exe()?;
    let executable = if current.file_name().is_some_and(|name| name == "nanocodex") {
        current
    } else {
        current
            .parent()
            .ok_or_else(|| io::Error::other("Cannot locate the installed Nanocodex client"))?
            .join("nanocodex")
    };
    let mut command = Command::new(executable);
    command
        .args(["hand", "connect", "--account-file"])
        .arg(&receipt.account_file)
        .arg("--managed-url")
        .arg(&receipt.origin)
        .env_remove("NANOCODEX_API_KEY")
        .env_remove("NC_API_KEY")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if receipt.credentials_changed {
        command.arg("--credentials-changed");
    }
    let mut child = command.spawn()?;
    match tokio::time::timeout(Duration::from_secs(60), child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Err(error)) => Err(error),
        Ok(Ok(_)) => Err(io::Error::other("Hand connection is pending")),
        Err(_) => {
            // kill_on_drop also covers cancellation while waiting for the service.
            let _ = child.start_kill();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Hand connection is pending",
            ))
        }
    }
}
