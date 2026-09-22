//! Play the publisher's existing Opus RTP through FFplay's jitter buffer and
//! platform audio output. No microphone or second remote peer is opened.
use super::{Result, Snapshot, media_candidates};
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::UdpSocket, sync::watch};
use webrtc::{track::track_remote::TrackRemote, util::Marshal};

struct Player {
    child: tokio::process::Child,
    socket: UdpSocket,
    destination: std::net::SocketAddr,
}
impl Player {
    async fn start(payload_type: u8) -> Result<Self> {
        Self::start_with(payload_type, |_| {}).await
    }
    async fn start_with(
        payload_type: u8,
        configure: impl Fn(&mut tokio::process::Command),
    ) -> Result<Self> {
        // Reserve an ephemeral port while preparing the local-only SDP.
        let reservation = UdpSocket::bind("127.0.0.1:0").await?;
        let destination = reservation.local_addr()?;
        let socket = UdpSocket::bind("127.0.0.1:0").await?;
        let description = sdp(destination.port(), payload_type);
        let mut last = None;
        drop(reservation);
        for program in media_candidates("ffplay") {
            let mut command = tokio::process::Command::new(&program);
            command
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nodisp",
                    "-autoexit",
                    "-protocol_whitelist",
                    "pipe,udp,rtp",
                    "-reorder_queue_size",
                    "64",
                    "-max_delay",
                    "60000",
                    "-probesize",
                    "32",
                    "-analyzeduration",
                    "0",
                    "-f",
                    "sdp",
                    "-i",
                    "pipe:0",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            #[cfg(target_os = "windows")]
            command.creation_flags(0x08000000);
            configure(&mut command);
            match command.spawn() {
                Ok(mut child) => {
                    child
                        .stdin
                        .take()
                        .ok_or("Audio player stdin unavailable")?
                        .write_all(description.as_bytes())
                        .await?;
                    // FFplay opens its RTP socket after consuming the SDP.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if let Some(status) = child.try_wait()? {
                        return Err(format!("Audio player exited ({status})").into());
                    }
                    return Ok(Self {
                        child,
                        socket,
                        destination,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => last = Some(error),
                Err(error) => {
                    return Err(format!("Could not start {}: {error}", program.display()).into());
                }
            }
        }
        Err(format!(
            "FFplay was not found; install the ffmpeg package ({})",
            last.unwrap()
        )
        .into())
    }
}
fn sdp(port: u16, payload_type: u8) -> String {
    format!(
        "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=Hand audio\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {port} RTP/AVP {payload_type}\r\na=rtpmap:{payload_type} opus/48000/2\r\na=fmtp:{payload_type} stereo=1;useinbandfec=1\r\na=source-filter: incl IN IP4 127.0.0.1 127.0.0.1\r\n"
    )
}
pub(super) async fn play(
    track: Arc<TrackRemote>,
    mut muted: watch::Receiver<bool>,
    output: watch::Sender<Snapshot>,
) -> Result<()> {
    if !track
        .codec()
        .capability
        .mime_type
        .eq_ignore_ascii_case("audio/opus")
    {
        return Err("Unsupported desktop audio codec (expected Opus)".into());
    }
    let mut player = None;
    loop {
        if *muted.borrow_and_update() {
            player = None; // kill_on_drop flushes all buffered sound on mute.
            output.send_modify(|s| s.audio = "muted".into());
        } else if player.is_none() {
            player = Some(Player::start(track.payload_type()).await?);
            output.send_modify(|s| s.audio = "on".into());
        }
        tokio::select! {
            result = muted.changed() => if result.is_err() { return Ok(()); },
            packet = track.read_rtp() => {
                let (packet, _) = packet?;
                if let Some(player) = &mut player {
                    if let Some(status) = player.child.try_wait()? {
                        return Err(format!("Audio output stopped ({status})").into());
                    }
                    player.socket.send_to(&packet.marshal()?, player.destination).await?;
                    output.send_if_modified(|s| {
                        // Publish a counter periodically, without repainting for every packet.
                        s.audio_packets += 1;
                        s.audio_packets % 50 == 0
                    });
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_sdp_preserves_negotiated_payload_and_stereo_on_loopback() {
        let description = sdp(43210, 111);
        assert!(description.contains("m=audio 43210 RTP/AVP 111\r\n"));
        assert!(description.contains("opus/48000/2"));
        assert!(description.contains("source-filter: incl IN IP4 127.0.0.1 127.0.0.1"));
        assert!(!description.contains("0.0.0.0"));
    }
    #[tokio::test]
    #[ignore = "requires local ffplay with the SDL disk audio driver"]
    async fn opus_reaches_audio_output() {
        use opusic_c::{Application, Channels, Encoder, SampleRate};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("audio.raw");
        let mut player = Player::start_with(111, |command| {
            command
                .env("SDL_AUDIODRIVER", "disk")
                .env("SDL_DISKAUDIOFILE", &path);
        })
        .await
        .unwrap();
        let mut encoder =
            Encoder::new(Channels::Stereo, SampleRate::Hz48000, Application::Audio).unwrap();
        let mut encoded = [0; 4000];
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        for sequence in 0..150u16 {
            tick.tick().await;
            let mut pcm = [0.0f32; 1920];
            for frame in 0..960 {
                let sample =
                    ((f32::from(sequence) * 960.0 + frame as f32) * 440.0 * std::f32::consts::TAU
                        / 48000.0)
                        .sin()
                        * 0.2;
                pcm[frame * 2] = sample;
                pcm[frame * 2 + 1] = sample;
            }
            let count = encoder.encode_float_to_slice(&pcm, &mut encoded).unwrap();
            let packet = webrtc::rtp::packet::Packet {
                header: webrtc::rtp::header::Header {
                    version: 2,
                    payload_type: 111,
                    sequence_number: sequence,
                    timestamp: u32::from(sequence) * 960,
                    ssrc: 123456,
                    ..Default::default()
                },
                payload: encoded[..count].to_vec().into(),
            };
            player
                .socket
                .send_to(&packet.marshal().unwrap(), player.destination)
                .await
                .unwrap();
            assert!(player.child.try_wait().unwrap().is_none());
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 48_000, "output only {} bytes", bytes.len());
        assert!(
            bytes.iter().filter(|&&b| b != 0).count() > 10_000,
            "silent playout"
        );
        eprintln!("Decoded Opus to {} PCM output bytes", bytes.len());
        #[cfg(unix)]
        let pid = nix::unistd::Pid::from_raw(player.child.id().unwrap() as i32);
        drop(player);
        #[cfg(unix)]
        tokio::time::timeout(Duration::from_secs(2), async {
            while nix::sys::signal::kill(pid, None).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("closing player must stop audio and reap its child");
    }
}
