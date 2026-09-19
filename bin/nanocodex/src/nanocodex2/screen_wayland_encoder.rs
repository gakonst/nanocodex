//! Waymote invokes this executable as its FFmpeg helper. Records are atomic at
//! Linux PIPE_BUF, so a killed helper cannot poison its replacement's framing.
use nix::libc;
use std::{
    io::{self, Write},
    os::fd::AsFd,
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const MAGIC: &[u8; 8] = b"NCH264C1";
const MAX_FRAME: usize = 8 * 1024 * 1024;
pub(crate) const HELPER_ENV: &str = "NANOCODEX_SCREEN_ENCODER_HELPER";

pub(crate) fn encoder_args(original: &[String], hardware: bool) -> Result<Vec<String>> {
    let value = |name: &str| {
        original
            .windows(2)
            .rev()
            .find(|p| p[0] == name)
            .map(|p| p[1].as_str())
    };
    let fps: u32 = value("-framerate").ok_or("missing frame rate")?.parse()?;
    let bitrate = value("-b:v").ok_or("missing bitrate")?;
    let kbps: u32 = bitrate
        .strip_suffix('k')
        .ok_or("invalid bitrate")?
        .parse()?;
    if !(1..=240).contains(&fps)
        || kbps == 0
        || value("-f") != Some("h264")
        || original.last().map(String::as_str) != Some("pipe:1")
    {
        return Err("unsupported screen encoder input".into());
    }
    let dimensions = value("-video_size").ok_or("missing raw screen dimensions")?;
    let (width, height) = dimensions
        .split_once('x')
        .ok_or("invalid screen dimensions")?;
    if width.parse::<u32>()? == 0 || height.parse::<u32>()? == 0 {
        return Err("invalid screen dimensions".into());
    }
    let scale = format!("scale={}", dimensions.replace('x', ":"));
    let mut args = Vec::new();
    let mut i = 0;
    while i < original.len() - 1 {
        let key = original[i].as_str();
        if key == "-re" {
            i += 1;
            continue;
        }
        if (key == "-vf" && original.get(i + 1) == Some(&scale))
            || (hardware && matches!(key, "-x264-params" | "-sc_threshold" | "-keyint_min"))
        {
            i += 2;
            continue;
        }
        let replacement = match key {
            "-maxrate" => Some(bitrate.to_owned()),
            "-bufsize" => Some(format!("{}k", (kbps / fps).max(1))),
            "-g" | "-keyint_min" => Some((fps / 2).max(1).to_string()),
            "-c:v" if hardware => Some("h264_nvenc".into()),
            "-preset" if hardware => Some("p3".into()),
            "-tune" if hardware => Some("ull".into()),
            "-pix_fmt" if hardware => Some("bgra".into()),
            _ => None,
        };
        args.push(key.to_owned());
        if let Some(v) = replacement {
            args.push(v);
            i += 2;
        } else {
            i += 1;
        }
    }
    if hardware {
        args.extend(
            [
                "-rc",
                "cbr",
                "-rc-lookahead",
                "0",
                "-zerolatency",
                "1",
                "-delay",
                "0",
                "-aud",
                "1",
            ]
            .map(str::to_owned),
        );
    }
    args.extend(["-flush_packets", "1", "pipe:1"].map(str::to_owned));
    Ok(args)
}
fn atomic_write(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    loop {
        match output.write(bytes) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Ok(n) if n == bytes.len() => return Ok(()),
            Ok(_) => return Err(io::ErrorKind::WriteZero.into()),
            Err(e) => return Err(e),
        }
    }
}
fn write_frame(output: &mut impl Write, frame: &[u8]) -> io::Result<()> {
    if frame.is_empty() || frame.len() > MAX_FRAME {
        return Err(io::ErrorKind::InvalidData.into());
    }
    for (i, chunk) in frame.chunks(4092).enumerate() {
        let mut record = [0u8; 4096];
        let last = (i + 1) * 4092 >= frame.len();
        let length = chunk.len() as u32 | if last { 0x80000000 } else { 0 };
        record[..4].copy_from_slice(&length.to_be_bytes());
        record[4..4 + chunk.len()].copy_from_slice(chunk);
        atomic_write(output, &record[..4 + chunk.len()])?;
    }
    Ok(())
}
async fn forward<M: tokio::io::AsyncRead + Unpin, V: tokio::io::AsyncRead + Unpin>(
    metadata: M,
    mut video: V,
    output: &mut impl Write,
) -> Result<()> {
    atomic_write(output, MAGIC)?;
    let mut reader = BufReader::new(metadata);
    let mut line = Vec::new();
    let mut frame = Vec::new();
    loop {
        line.clear();
        loop {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                if line.is_empty() {
                    return Ok(());
                }
                return Err("truncated encoder metadata".into());
            }
            let newline = available.iter().position(|b| *b == b'\n');
            let count = newline.map_or(available.len(), |n| n + 1);
            if line.len() + count > 16384 {
                return Err("oversized encoder metadata".into());
            }
            line.extend_from_slice(&available[..count]);
            reader.consume(count);
            if newline.is_some() {
                break;
            }
        }
        if line.starts_with(b"#") {
            continue;
        }
        let fields: Vec<_> = std::str::from_utf8(&line)?.split(',').collect();
        if fields.len() < 6 || fields[0].trim() != "0" {
            return Err("invalid encoder metadata".into());
        }
        let size: usize = fields[4].trim().parse()?;
        if !(1..=MAX_FRAME).contains(&size) {
            return Err("invalid encoder frame size".into());
        }
        frame.resize(size, 0);
        video.read_exact(&mut frame).await?;
        write_frame(output, &frame)?;
    }
}
// Waymote replaces helpers with SIGKILL during resize. The encoder must not
// retain inherited stdout and mix old frames into its replacement's stream.
fn parent_bound(command: &mut tokio::process::Command) -> &mut tokio::process::Command {
    let parent = nix::unistd::getpid();
    // SAFETY: this post-fork hook only performs Linux prctl/getppid/_exit
    // syscalls and errno reads; it never allocates or takes a lock. The parent
    // check closes the race where the parent exits before PDEATHSIG is armed.
    #[allow(unsafe_code)]
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) < 0 {
                return Err(io::Error::last_os_error());
            }
            if nix::unistd::getppid() != parent {
                libc::_exit(1);
            }
            Ok(())
        });
    }
    command
}
pub(crate) async fn run(original: Vec<String>) -> Result<()> {
    let mode = std::env::var("NANOCODEX_VIDEO_ENCODER").unwrap_or("auto".into());
    if !matches!(mode.as_str(), "auto" | "software" | "nvenc") {
        return Err("invalid screen encoder mode".into());
    }
    let mut hardware = false;
    if mode != "software" {
        let candidate = encoder_args(&original, true)?;
        let size = candidate
            .windows(2)
            .find(|p| p[0] == "-video_size")
            .ok_or("missing screen dimensions")?[1]
            .clone();
        let start = candidate
            .iter()
            .position(|a| a == "-i")
            .ok_or("missing raw input")?
            + 2;
        let mut probe = tokio::process::Command::new("ffmpeg");
        probe
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                &format!("color=size={size}:rate=60"),
            ])
            .args(&candidate[start..candidate.len() - 1])
            .args(["-frames:v", "1", "pipe:1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        hardware = matches!(tokio::time::timeout(Duration::from_secs(5),parent_bound(&mut probe).status()).await,Ok(Ok(s)) if s.success());
        if mode == "nvenc" && !hardware {
            return Err("NVENC screen encoder unavailable".into());
        }
    }
    let args = encoder_args(&original, hardware)?;
    eprintln!(
        "Screen encoder: {}",
        if hardware {
            "NVIDIA NVENC"
        } else {
            "software H.264"
        }
    );
    if std::env::var("NANOCODEX_SCREEN_FRAME_BOUNDARIES").as_deref() == Ok("annexb") {
        if !parent_bound(
            tokio::process::Command::new("ffmpeg")
                .args(args)
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .kill_on_drop(true),
        )
        .status()
        .await?
        .success()
        {
            return Err("screen encoder failed".into());
        }
        return Ok(());
    }
    let directory = tempfile::Builder::new()
        .prefix("nanocodex-encoder-")
        .tempdir()?;
    let path = directory.path().join("frames");
    let listener = tokio::net::UnixListener::bind(&path)?;
    let output = format!(
        "[f=framecrc:flush_packets=1]unix://{}|[f=h264:flush_packets=1]pipe:1",
        path.display()
    );
    let mut args = args;
    let at = args
        .windows(2)
        .rposition(|v| v[0] == "-f" && v[1] == "h264")
        .ok_or("missing encoder output")?;
    args[at + 1] = "tee".into();
    args.pop();
    args.extend(["-map".into(), "0:v:0".into(), output]);
    let mut child = parent_bound(
        tokio::process::Command::new("ffmpeg")
            .args(["-probesize", "32", "-analyzeduration", "0"])
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true),
    )
    .spawn()?;
    let video = child.stdout.take().ok_or("encoder stdout unavailable")?;
    let (metadata, _) = tokio::select! {result=listener.accept()=>result?,result=child.wait()=>{return Err(format!("encoder exited before metadata: {result:?}").into());}};
    // File writes bypass Rust stdout buffering. Each syscall contains a complete
    // <=PIPE_BUF record; never resume a short record across encoder restarts.
    // Clone atomically with CLOEXEC so another thread cannot inherit the pipe
    // between a dup and a separate flag update.
    let fd = io::stdout().as_fd().try_clone_to_owned()?;
    let mut stdout = std::fs::File::from(fd);
    let result = forward(metadata, video, &mut stdout).await;
    if result.is_err() {
        let _ = child.kill().await;
    }
    let status = child.wait().await?;
    result?;
    if !status.success() {
        return Err("screen encoder failed".into());
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_fast_encoder_policy_and_nvenc_options() {
        let original="-f rawvideo -video_size 1920x1080 -framerate 60 -i pipe:0 -re -vf scale=1920:1080 -c:v libx264 -pix_fmt yuv420p -preset ultrafast -tune zerolatency -x264-params repeat-headers=1:aud=1 -b:v 6000k -maxrate 9000k -bufsize 12000k -g 120 -keyint_min 60 -sc_threshold 0 -f h264 pipe:1".split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        for hardware in [false, true] {
            let args = encoder_args(&original, hardware).unwrap();
            let value = |key: &str| args.windows(2).find(|p| p[0] == key).map(|p| p[1].as_str());
            assert!(!args.iter().any(|v| v == "-re" || v == "-vf"));
            assert_eq!(value("-bufsize"), Some("100k"));
            assert_eq!(value("-g"), Some("30"));
            assert_eq!(
                value("-c:v"),
                Some(if hardware { "h264_nvenc" } else { "libx264" })
            );
            assert_eq!(
                value("-pix_fmt"),
                Some(if hardware { "bgra" } else { "yuv420p" })
            );
        }
        assert!(encoder_args(&[], false).is_err());
    }
    #[derive(Default)]
    struct Writes(Vec<Vec<u8>>);
    impl Write for Writes {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.push(b.to_vec());
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn records_are_atomic_and_final_flag_is_exact() {
        let mut output = Writes::default();
        write_frame(&mut output, &vec![7; 8185]).unwrap();
        assert_eq!(
            output.0.iter().map(Vec::len).collect::<Vec<_>>(),
            [4096, 4096, 5]
        );
        assert_eq!(
            u32::from_be_bytes(output.0[2][..4].try_into().unwrap()),
            0x80000001
        );
    }
    #[tokio::test]
    async fn metadata_preserves_single_frame_without_lookahead() {
        let mut output = Writes::default();
        forward(
            b"#header\n0, 0, 0, 1, 5, 0x0\n".as_slice(),
            b"frame".as_slice(),
            &mut output,
        )
        .await
        .unwrap();
        assert_eq!(
            output.0,
            [
                MAGIC.to_vec(),
                [0x80, 0, 0, 5, b'f', b'r', b'a', b'm', b'e'].to_vec()
            ]
        );
    }
    #[tokio::test]
    async fn partial_frame_is_not_published() {
        let mut output = Writes::default();
        assert!(
            forward(
                b"0,0,0,1,8,0\n".as_slice(),
                b"short".as_slice(),
                &mut output
            )
            .await
            .is_err()
        );
        assert_eq!(output.0, [MAGIC.to_vec()]);
    }
}
