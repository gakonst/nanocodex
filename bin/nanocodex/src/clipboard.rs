//! Text clipboard routing shared by both terminal clients.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use std::io::Write;
use std::process::{Command, Stdio};

const OSC52_MAX_RAW_BYTES: usize = 100_000;

pub(crate) fn copy_to_clipboard(text: &str) -> Result<(), String> {
    copy_to_clipboard_with(
        text,
        CopyEnvironment {
            ssh_session: is_ssh_session(),
            tmux_session: is_tmux_session(),
        },
        native_clipboard_copy,
        tmux_clipboard_copy,
        osc52_copy,
    )
}

#[derive(Clone, Copy)]
struct CopyEnvironment {
    ssh_session: bool,
    tmux_session: bool,
}

fn copy_to_clipboard_with(
    text: &str,
    environment: CopyEnvironment,
    native_copy: impl Fn(&str) -> Result<(), String>,
    tmux_copy: impl Fn(&str) -> Result<(), String>,
    osc52_copy: impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if environment.ssh_session {
        return terminal_clipboard_copy(text, environment.tmux_session, &tmux_copy, &osc52_copy);
    }

    native_copy(text).or_else(|native_error| {
        tracing::warn!(
            %native_error,
            "native clipboard copy failed; falling back to the terminal"
        );
        terminal_clipboard_copy(text, environment.tmux_session, &tmux_copy, &osc52_copy).map_err(
            |terminal_error| {
                format!("native clipboard: {native_error}; terminal fallback: {terminal_error}")
            },
        )
    })
}

fn terminal_clipboard_copy(
    text: &str,
    tmux_session: bool,
    tmux_copy: &impl Fn(&str) -> Result<(), String>,
    osc52_copy: &impl Fn(&str) -> Result<(), String>,
) -> Result<(), String> {
    if tmux_session {
        return tmux_copy(text).or_else(|tmux_error| {
            tracing::warn!(%tmux_error, "tmux clipboard copy failed; falling back to OSC 52");
            osc52_copy(text).map_err(|osc52_error| {
                format!("tmux clipboard: {tmux_error}; OSC 52 fallback: {osc52_error}")
            })
        });
    }
    osc52_copy(text)
}

fn is_ssh_session() -> bool {
    std::env::var_os("SSH_TTY").is_some() || std::env::var_os("SSH_CONNECTION").is_some()
}

fn is_tmux_session() -> bool {
    std::env::var_os("TMUX").is_some() || std::env::var_os("TMUX_PANE").is_some()
}

#[cfg(target_os = "macos")]
fn native_clipboard_copy(text: &str) -> Result<(), String> {
    write_command("pbcopy", &[], text)
}

#[cfg(target_os = "windows")]
fn native_clipboard_copy(text: &str) -> Result<(), String> {
    write_command("clip.exe", &[], text)
}

#[cfg(target_os = "linux")]
fn native_clipboard_copy(text: &str) -> Result<(), String> {
    let commands: [(&str, &[&str]); 4] = [
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
        ("clip.exe", &[]),
    ];
    let mut errors = Vec::with_capacity(commands.len());
    for (program, arguments) in commands {
        match write_command(program, arguments, text) {
            Ok(()) => return Ok(()),
            Err(error) => errors.push(error),
        }
    }
    Err(errors.join("; "))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn native_clipboard_copy(_text: &str) -> Result<(), String> {
    Err("native clipboard copy is unavailable on this platform".to_owned())
}

fn tmux_clipboard_copy(text: &str) -> Result<(), String> {
    tmux_clipboard_ready(
        || tmux_command_output(&["show-options", "-gv", "set-clipboard"]),
        || tmux_command_output(&["info"]),
    )?;
    write_command("tmux", &["load-buffer", "-w", "-"], text)
}

fn tmux_clipboard_ready(
    set_clipboard: impl FnOnce() -> Result<String, String>,
    tmux_info: impl FnOnce() -> Result<String, String>,
) -> Result<(), String> {
    if set_clipboard()?.trim() == "off" {
        return Err("tmux clipboard forwarding is disabled".to_owned());
    }
    if tmux_info()?
        .lines()
        .any(|line| line.contains("Ms: [missing]"))
    {
        return Err("tmux clipboard forwarding is unavailable: missing Ms capability".to_owned());
    }
    Ok(())
}

fn tmux_command_output(arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("tmux")
        .args(arguments)
        .output()
        .map_err(|error| format!("failed to spawn tmux: {error}"))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| format!("tmux output was not UTF-8: {error}"))
    } else {
        Err(command_failure("tmux", output.status, &output.stderr))
    }
}

fn write_command(program: &str, arguments: &[&str], text: &str) -> Result<(), String> {
    let mut child = Command::new(program)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to spawn {program}: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to open {program} stdin"));
    };
    if let Err(error) = stdin.write_all(text.as_bytes()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("failed to write to {program}: {error}"));
    }
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to wait for {program}: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(program, output.status, &output.stderr))
    }
}

fn command_failure(program: &str, status: std::process::ExitStatus, stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if stderr.is_empty() {
        format!("{program} exited with status {status}")
    } else {
        format!("{program} failed: {stderr}")
    }
}

fn osc52_copy(text: &str) -> Result<(), String> {
    let sequence = osc52_sequence(text, is_tmux_session())?;
    #[cfg(unix)]
    if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty")
        && write_osc52(tty, &sequence).is_ok()
    {
        return Ok(());
    }
    write_osc52(std::io::stdout().lock(), &sequence)
}

fn osc52_sequence(text: &str, tmux: bool) -> Result<String, String> {
    if text.len() > OSC52_MAX_RAW_BYTES {
        return Err(format!(
            "OSC 52 payload too large ({} bytes; max {OSC52_MAX_RAW_BYTES})",
            text.len()
        ));
    }
    let encoded = BASE64_STANDARD.encode(text);
    if tmux {
        Ok(format!("\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\"))
    } else {
        Ok(format!("\x1b]52;c;{encoded}\x07"))
    }
}

fn write_osc52(mut output: impl Write, sequence: &str) -> Result<(), String> {
    output
        .write_all(sequence.as_bytes())
        .map_err(|error| format!("failed to write OSC 52: {error}"))?;
    output
        .flush()
        .map_err(|error| format!("failed to flush OSC 52: {error}"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::{CopyEnvironment, copy_to_clipboard_with, osc52_sequence, tmux_clipboard_ready};

    #[cfg(unix)]
    #[test]
    fn clipboard_command_receives_utf8_text_and_eof() {
        super::write_command(
            "/bin/sh",
            &["-c", "test \"$(cat)\" = 'copy 短 this'"],
            "copy 短 this",
        )
        .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn clipboard_command_failure_reports_stderr() {
        let error = super::write_command(
            "/bin/sh",
            &[
                "-c",
                "cat >/dev/null; echo clipboard-unavailable >&2; exit 1",
            ],
            "selected text",
        )
        .unwrap_err();
        assert!(error.contains("clipboard-unavailable"));
    }

    #[test]
    fn local_copy_prefers_the_native_clipboard() {
        let native_calls = Cell::new(0);
        let tmux_calls = Cell::new(0);
        let osc52_calls = Cell::new(0);
        copy_to_clipboard_with(
            "hello",
            CopyEnvironment {
                ssh_session: false,
                tmux_session: true,
            },
            |_| {
                native_calls.set(native_calls.get() + 1);
                Ok(())
            },
            |_| {
                tmux_calls.set(tmux_calls.get() + 1);
                Ok(())
            },
            |_| {
                osc52_calls.set(osc52_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(native_calls.get(), 1);
        assert_eq!(tmux_calls.get(), 0);
        assert_eq!(osc52_calls.get(), 0);
    }

    #[test]
    fn local_copy_does_not_depend_on_terminal_clipboard_support() {
        copy_to_clipboard_with(
            "selected text",
            CopyEnvironment {
                ssh_session: false,
                tmux_session: false,
            },
            |text| {
                assert_eq!(text, "selected text");
                Ok(())
            },
            |_| panic!("local copy must use the native clipboard first"),
            |_| panic!("a successful OSC 52 write cannot confirm clipboard support"),
        )
        .unwrap();
    }

    #[test]
    fn ssh_copy_uses_the_terminal_and_skips_the_remote_clipboard() {
        copy_to_clipboard_with(
            "selected text",
            CopyEnvironment {
                ssh_session: true,
                tmux_session: false,
            },
            |_| panic!("must not copy to the remote host clipboard"),
            |_| panic!("not inside tmux"),
            |text| {
                assert_eq!(text, "selected text");
                Ok(())
            },
        )
        .unwrap();
    }

    #[test]
    fn native_failure_can_copy_through_the_terminal() {
        let terminal_calls = Cell::new(0);
        copy_to_clipboard_with(
            "selected text",
            CopyEnvironment {
                ssh_session: false,
                tmux_session: false,
            },
            |_| Err("native unavailable".to_owned()),
            |_| panic!("not inside tmux"),
            |text| {
                assert_eq!(text, "selected text");
                terminal_calls.set(terminal_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(terminal_calls.get(), 1);
    }

    #[test]
    fn native_failure_falls_back_and_preserves_errors_when_both_fail() {
        let result = copy_to_clipboard_with(
            "selected text",
            CopyEnvironment {
                ssh_session: false,
                tmux_session: false,
            },
            |_| Err("native unavailable".to_owned()),
            |_| panic!("not inside tmux"),
            |_| Err("terminal unavailable".to_owned()),
        );
        let error = result.unwrap_err();
        assert!(error.contains("native unavailable"));
        assert!(error.contains("terminal unavailable"));
    }

    #[test]
    fn ssh_inside_tmux_prefers_tmux_and_skips_the_remote_native_clipboard() {
        let native_calls = Cell::new(0);
        let tmux_calls = Cell::new(0);
        copy_to_clipboard_with(
            "hello",
            CopyEnvironment {
                ssh_session: true,
                tmux_session: true,
            },
            |_| {
                native_calls.set(native_calls.get() + 1);
                Ok(())
            },
            |_| {
                tmux_calls.set(tmux_calls.get() + 1);
                Ok(())
            },
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(native_calls.get(), 0);
        assert_eq!(tmux_calls.get(), 1);
    }

    #[test]
    fn tmux_failure_falls_back_to_a_tmux_wrapped_osc52_sequence() {
        let osc52_calls = Cell::new(0);
        copy_to_clipboard_with(
            "hello",
            CopyEnvironment {
                ssh_session: true,
                tmux_session: true,
            },
            |_| Ok(()),
            |_| Err("blocked".to_owned()),
            |_| {
                osc52_calls.set(osc52_calls.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(osc52_calls.get(), 1);
        assert_eq!(
            osc52_sequence("hello", true).unwrap(),
            "\x1bPtmux;\x1b\x1b]52;c;aGVsbG8=\x07\x1b\\"
        );
    }

    #[test]
    fn tmux_external_clipboard_mode_is_supported() {
        assert_eq!(
            tmux_clipboard_ready(
                || Ok("external\n".to_owned()),
                || Ok("193: Ms: (string) \\033]52;%p1%s;%p2%s\\a\n".to_owned()),
            ),
            Ok(())
        );
    }

    #[test]
    fn tmux_without_clipboard_forwarding_is_rejected() {
        assert_eq!(
            tmux_clipboard_ready(
                || Ok("off\n".to_owned()),
                || panic!("tmux info should not be queried"),
            ),
            Err("tmux clipboard forwarding is disabled".to_owned())
        );
        assert_eq!(
            tmux_clipboard_ready(
                || Ok("external\n".to_owned()),
                || Ok("193: Ms: [missing]\n".to_owned()),
            ),
            Err("tmux clipboard forwarding is unavailable: missing Ms capability".to_owned())
        );
    }
}
