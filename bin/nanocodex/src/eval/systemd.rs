use std::{
    env,
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::Command,
};

use eyre::{Result, WrapErr as _, bail, eyre};
use nanocodex_eval::{EvaluationObserver, coordinator::CoordinatorClient};

use super::profile::default_state_dir;

pub(super) fn install(
    profile: &str,
    config: &Path,
    state_dir: Option<&Path>,
    coordinator: Option<&str>,
    runtime_dir: Option<&Path>,
) -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("--systemd is supported only on Linux");
    }

    let cwd = env::current_dir().wrap_err("failed to resolve current directory")?;
    let config = absolute_existing(config, "runtime helper config")?;
    let state_dir = if coordinator.is_some() {
        None
    } else {
        let state_dir = state_dir.map_or_else(default_state_dir, |path| Ok(path.to_path_buf()))?;
        let state_dir = std::path::absolute(state_dir)?;
        EvaluationObserver::open(&state_dir, profile)?;
        Some(state_dir.canonicalize().wrap_err_with(|| {
            format!("failed to resolve state directory {}", state_dir.display())
        })?)
    };
    if let Some(coordinator) = coordinator {
        CoordinatorClient::new(coordinator)?;
    }

    let runtime_dir = match runtime_dir {
        None => state_dir.as_ref().map_or_else(
            || config.parent().unwrap_or(&cwd).join(".nanocodex-runtime"),
            |state| state.join("runtime"),
        ),
        Some(path) => std::path::absolute(path)?,
    };
    let temporary_directory = runtime_dir.join("tmp");
    fs::create_dir_all(&temporary_directory).wrap_err_with(|| {
        format!(
            "failed to create benchmark runtime directory {}",
            temporary_directory.display()
        )
    })?;
    let runtime_dir = runtime_dir.canonicalize().wrap_err_with(|| {
        format!(
            "failed to resolve benchmark runtime directory {}",
            runtime_dir.display()
        )
    })?;

    let executable = env::current_exe().wrap_err("failed to resolve the nanocodex executable")?;
    let arguments = service_arguments(&config, state_dir.as_deref());
    let unit_name = unit_name(profile);
    let unit = render_unit(
        &executable,
        &arguments,
        config.parent().unwrap_or(&cwd),
        &runtime_dir,
        &temporary_directory,
    )?;
    let unit_path = user_unit_directory()?.join(&unit_name);
    fs::create_dir_all(
        unit_path
            .parent()
            .ok_or_else(|| eyre!("systemd unit path has no parent"))?,
    )?;
    write_atomic(&unit_path, unit.as_bytes())?;
    systemctl(["daemon-reload"])?;
    systemctl(["enable", unit_name.as_str()])?;
    systemctl(["restart", unit_name.as_str()])?;

    println!("Installed and started {unit_name}");
    println!("  systemctl --user status {unit_name}");
    println!("  journalctl --user --unit {unit_name} --follow");
    Ok(())
}

fn service_arguments(config: &Path, state_dir: Option<&Path>) -> Vec<OsString> {
    let mut source = env::args_os().skip(1);
    let mut arguments = Vec::new();
    while let Some(argument) = source.next() {
        if argument == "--systemd" {
            continue;
        }
        if ["--runtime-dir", "--config", "--state-dir"]
            .iter()
            .any(|option| argument == *option)
        {
            source.next();
            continue;
        }
        if argument.to_str().is_some_and(|argument| {
            ["--runtime-dir=", "--config=", "--state-dir="]
                .iter()
                .any(|option| argument.starts_with(option))
        }) {
            continue;
        }
        arguments.push(argument);
    }
    arguments.push(OsString::from("--config"));
    arguments.push(config.as_os_str().to_owned());
    if let Some(state_dir) = state_dir {
        arguments.push(OsString::from("--state-dir"));
        arguments.push(state_dir.as_os_str().to_owned());
    }
    if !arguments.iter().any(|argument| argument == "--headless") {
        arguments.push(OsString::from("--headless"));
    }
    arguments
}

fn render_unit(
    executable: &Path,
    arguments: &[OsString],
    working_directory: &Path,
    runtime_directory: &Path,
    temporary_directory: &Path,
) -> Result<String> {
    let command = render_command(executable, arguments)?;
    Ok(format!(
        "[Unit]\n\
         Description=Nanocodex neural evaluation controller\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         StartLimitIntervalSec=0\n\
         \n\
         [Service]\n\
         Type=simple\n\
         WorkingDirectory={}\n\
         Environment={}\n\
         Environment={}\n\
         ExecStart={command}\n\
         Restart=on-failure\n\
         RestartSec=1s\n\
         KillMode=control-group\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        escape_path(working_directory.as_os_str())?,
        quote(
            OsString::from(format!("NANOCODEX_HOME={}", runtime_directory.display())).as_os_str()
        )?,
        quote(OsString::from(format!("TMPDIR={}", temporary_directory.display())).as_os_str())?,
    ))
}

fn render_command(executable: &Path, arguments: &[OsString]) -> Result<String> {
    let mut command = quote(executable.as_os_str())?;
    for argument in arguments {
        write!(command, " {}", quote(argument)?)?;
    }
    Ok(command)
}

fn escape_path(value: &OsStr) -> Result<String> {
    let value = value
        .to_str()
        .ok_or_else(|| eyre!("systemd paths must be valid UTF-8"))?;
    let mut escaped = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'/' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' | b':' | b'_' | b'.' | b'-' => {
                escaped.push(char::from(byte));
            }
            b'%' => escaped.push_str("%%"),
            byte => write!(escaped, "\\x{byte:02x}")?,
        }
    }
    Ok(escaped)
}

fn quote(value: &OsStr) -> Result<String> {
    let value = value
        .to_str()
        .ok_or_else(|| eyre!("systemd arguments must be valid UTF-8"))?;
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

fn unit_name(profile: &str) -> String {
    let profile = profile
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    format!("nanocodex-benchmark-{}.service", profile.trim_matches('-'))
}

fn user_unit_directory() -> Result<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(config_home).join("systemd/user"));
    }
    let home = env::var_os("HOME")
        .ok_or_else(|| eyre!("HOME is not set; cannot install a user systemd service"))?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

fn absolute_existing(path: &Path, description: &str) -> Result<PathBuf> {
    std::path::absolute(path)?
        .canonicalize()
        .wrap_err_with(|| format!("failed to resolve {description} {}", path.display()))
}

fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| eyre!("systemd unit path has no parent"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(contents)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .wrap_err_with(|| format!("failed to install systemd unit {}", path.display()))?;
    Ok(())
}

fn systemctl<const N: usize>(arguments: [&str; N]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()
        .wrap_err("failed to execute systemctl --user")?;
    if !output.status.success() {
        bail!(
            "systemctl --user failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}
