//! FFmpeg command-line compatibility of the Hand encoder and the CLI viewer.

use std::{process::Stdio, time::Duration};

/// The frame-timing option of the FFmpeg that `command` runs: `-fps_mode`
/// (FFmpeg >= 5.1), or its predecessor `-vsync` with the same values for FFmpeg
/// 4.x such as Ubuntu 22.04's 4.4. The version probe is bounded; a missing,
/// slow or unrecognized FFmpeg (for example a development build) gets
/// `-fps_mode`, and its own launch then reports any failure.
pub async fn fps_mode_option(command: &std::process::Command) -> &'static str {
    let mut probe = tokio::process::Command::new(command.get_program());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => probe.env(key, value),
            None => probe.env_remove(key),
        };
    }
    if let Some(directory) = command.get_current_dir() {
        probe.current_dir(directory);
    }
    probe
        .args(["-hide_banner", "-version"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    probe.creation_flags(0x0800_0000);
    match tokio::time::timeout(Duration::from_secs(2), probe.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            option_for_version(&String::from_utf8_lossy(&output.stdout))
        }
        _ => "-fps_mode",
    }
}

fn option_for_version(version: &str) -> &'static str {
    let release = version
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("ffmpeg version "))
        .map(|release| release.trim_start_matches('n'))
        .unwrap_or_default();
    let mut numbers = release.split(|c: char| !c.is_ascii_digit());
    let major = numbers.next().and_then(|part| part.parse::<u32>().ok());
    let minor = numbers.next().and_then(|part| part.parse::<u32>().ok());
    match (major, minor.unwrap_or(0)) {
        (Some(major), minor) if major < 5 || (major == 5 && minor < 1) => "-vsync",
        _ => "-fps_mode",
    }
}
