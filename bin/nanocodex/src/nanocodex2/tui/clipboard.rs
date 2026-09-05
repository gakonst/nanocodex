// Derived from clabby/tact; modified for Nanocodex2.
// SPDX-License-Identifier: Apache-2.0

//! Native clipboard access at the terminal boundary.

use arboard::Clipboard;
use base64::{Engine, engine::general_purpose::STANDARD};
use png::{BitDepth, ColorType, Encoder, EncodingError};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
};
use thiserror::Error;

pub(crate) fn copy_to_tmux(text: &str) -> Result<(), TmuxCopyError> {
    copy_with_tmux(text, Path::new("tmux"))
}

#[derive(Debug, Error)]
pub(crate) enum TmuxCopyError {
    #[error("could not launch {program}: {source}")]
    Launch {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not write to {program}: {source}")]
    Write {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not wait for {program}: {source}")]
    Wait {
        program: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{program} exited with {status}")]
    Exit {
        program: PathBuf,
        status: ExitStatus,
    },
}

fn copy_with_tmux(text: &str, program: &Path) -> Result<(), TmuxCopyError> {
    let mut child = Command::new(program)
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| TmuxCopyError::Launch {
            program: program.to_path_buf(),
            source,
        })?;
    let write_result = child
        .stdin
        .take()
        .expect("piped tmux stdin must be available")
        .write_all(text.as_bytes())
        .map_err(|source| TmuxCopyError::Write {
            program: program.to_path_buf(),
            source,
        });
    let status = child.wait().map_err(|source| TmuxCopyError::Wait {
        program: program.to_path_buf(),
        source,
    })?;
    write_result?;
    if status.success() {
        Ok(())
    } else {
        Err(TmuxCopyError::Exit {
            program: program.to_path_buf(),
            status,
        })
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn copy_text(text: &str) -> Result<(), arboard::Error> {
    Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text))
}

#[cfg(target_os = "macos")]
pub(crate) fn copy_text(text: &str) -> Result<(), CopyTextError> {
    match copy_with_pbcopy(text, "/usr/bin/pbcopy") {
        Ok(()) => Ok(()),
        Err(pbcopy) => Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(text))
            .map_err(|arboard| CopyTextError { pbcopy, arboard }),
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Error)]
#[error("pbcopy failed: {pbcopy}; native pasteboard fallback failed: {arboard}")]
pub(crate) struct CopyTextError {
    pbcopy: PbcopyError,
    arboard: arboard::Error,
}

#[cfg(target_os = "macos")]
#[derive(Debug, Error)]
enum PbcopyError {
    #[error("could not launch {program}: {source}")]
    Launch {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("could not write to {program}: {source}")]
    Write {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("could not wait for {program}: {source}")]
    Wait {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("{program} exited with {status}")]
    Exit { program: String, status: ExitStatus },
}

#[cfg(target_os = "macos")]
fn copy_with_pbcopy(text: &str, program: &str) -> Result<(), PbcopyError> {
    let mut child = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| PbcopyError::Launch {
            program: program.to_owned(),
            source,
        })?;
    let write_result = child
        .stdin
        .take()
        .expect("piped pbcopy stdin must be available")
        .write_all(text.as_bytes())
        .map_err(|source| PbcopyError::Write {
            program: program.to_owned(),
            source,
        });
    let status = child.wait().map_err(|source| PbcopyError::Wait {
        program: program.to_owned(),
        source,
    })?;
    write_result?;
    if status.success() {
        Ok(())
    } else {
        Err(PbcopyError::Exit {
            program: program.to_owned(),
            status,
        })
    }
}

pub(crate) fn image_data_url() -> Option<String> {
    let mut clipboard = Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;
    encode_png(image.width, image.height, image.bytes.as_ref())
        .ok()
        .map(|png| format!("data:image/png;base64,{}", STANDARD.encode(png)))
}

fn encode_png(width: usize, height: usize, pixels: &[u8]) -> Result<Vec<u8>, EncodingError> {
    let width = u32::try_from(width).map_err(|_| EncodingError::LimitsExceeded)?;
    let height = u32::try_from(height).map_err(|_| EncodingError::LimitsExceeded)?;
    let mut png = Vec::new();
    let mut encoder = Encoder::new(&mut png, width, height);
    encoder.set_color(ColorType::Rgba);
    encoder.set_depth(BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    use super::copy_with_pbcopy;
    use super::{copy_with_tmux, encode_png};
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn clipboard_pixels_are_encoded_as_png() {
        let encoded = encode_png(1, 1, &[255, 0, 0, 255]).unwrap();

        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn tmux_copy_loads_buffer_from_stdin() {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("tmux");
        fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/args\"\n\
             cat > \"$(dirname \"$0\")/buffer\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();

        copy_with_tmux("copy me\n", &program).unwrap();

        assert_eq!(
            fs::read_to_string(directory.path().join("args")).unwrap(),
            "load-buffer\n-w\n-\n"
        );
        assert_eq!(
            fs::read(directory.path().join("buffer")).unwrap(),
            b"copy me\n"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn pbcopy_launch_failure_identifies_the_program() {
        let program = "/tact-test/missing-pbcopy";
        let error = copy_with_pbcopy("copy me", program).unwrap_err();

        assert!(matches!(
            error,
            super::PbcopyError::Launch {
                program: failed_program,
                ..
            } if failed_program == program
        ));
    }
}
