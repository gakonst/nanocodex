use std::{
    collections::VecDeque,
    sync::{Arc, OnceLock},
    time::Duration,
};

use opusic_c::{
    Application as OpusApplication, Channels as OpusChannels, Decoder as OpusDecoder,
    Encoder as OpusEncoder, InbandFec as OpusInbandFec, SampleRate as OpusSampleRate,
    Signal as OpusSignal,
};
use reqwest::{Client, StatusCode, header::LOCATION};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::{HeaderValue, header},
};
use tracing::{debug, trace, warn};
use url::Url;
use webrtc::{
    api::{
        APIBuilder,
        interceptor_registry::register_default_interceptors,
        media_engine::{MIME_TYPE_OPUS, MediaEngine},
    },
    interceptor::registry::Registry,
    media::{Sample, io::sample_builder::SampleBuilder},
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
    rtp::codecs::opus::OpusPacket,
    rtp_transceiver::rtp_codec::{RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType},
    track::track_local::{TrackLocal, track_local_static_sample::TrackLocalStaticSample},
};

use super::{
    CONNECT_TIMEOUT, EVENT_CAPACITY, RealtimeAudio, RealtimeError, RealtimeInitialItem,
    RealtimeVersion, RealtimeVoice, Socket, map_websocket_error,
};
use crate::{OpenAiAuth, OpenAiAuthSnapshot, connector::connect_async};

const DATA_CHANNEL_LABEL: &str = "oai-events";
const CALL_BODY_LIMIT: usize = 1024 * 1024;
const ERROR_BODY_LIMIT: usize = 8 * 1024;
const WEBRTC_INPUT_FRAME_SAMPLES: usize = 480;
const WEBRTC_MAX_FRAME_SAMPLES: usize = 2_880;
const WEBRTC_REORDER_PACKETS: u16 = 3;
const WEBRTC_REORDER_DELAY: Duration = Duration::from_millis(60);
const WEBRTC_MAX_CONCEALED_PACKETS: u16 = 6;
const WEBRTC_EXPECTED_PACKET_LOSS_PERCENT: u8 = 5;
const OPUS_PACKET_CAPACITY: usize = 4_000;
const OPENAI_REALTIME_BASE: &str = "https://api.openai.com/v1";
const ATTESTATION_UNAVAILABLE: &str = r#"{"v":1,"s":1}"#;
const MULTIPART_BOUNDARY: &str = "----nanocodex-realtime-boundary";

pub(super) struct WebRtcConnection {
    pub(super) socket: Socket,
    pub(super) media: WebRtcMedia,
    pub(super) sideband: WebRtcSideband,
}

pub(super) struct PreparedCallerSdpCall {
    pub(super) sdp: String,
    pub(super) sideband: WebRtcSideband,
    pub(super) initial_update: Option<Value>,
}

pub(super) struct ExistingCallConnection {
    pub(super) socket: Socket,
    pub(super) sideband: WebRtcSideband,
}

#[derive(Clone)]
pub(super) struct WebRtcSideband {
    auth: OpenAiAuthSnapshot,
    endpoint: Url,
    attestation_header: String,
    session_id: Option<String>,
    version: RealtimeVersion,
}

impl WebRtcSideband {
    fn from_config(
        config: &ConnectConfig<'_>,
        auth: OpenAiAuthSnapshot,
        call_id: &str,
    ) -> Result<Self, RealtimeError> {
        Ok(Self {
            auth,
            endpoint: sideband_endpoint(config.websocket_url, call_id, config.version)?,
            attestation_header: config
                .attestation_header
                .unwrap_or(ATTESTATION_UNAVAILABLE)
                .to_owned(),
            session_id: config.session_id.map(str::to_owned),
            version: config.version,
        })
    }

    pub(super) async fn reconnect(&self) -> Result<Socket, RealtimeError> {
        connect_sideband(self)
            .await
            .map(|(socket, _response)| socket)
    }

    #[cfg(test)]
    pub(super) fn for_test(endpoint: Url) -> Self {
        Self {
            auth: OpenAiAuthSnapshot::new(
                crate::OpenAiAuthMode::ApiKey,
                "test",
                Option::<String>::None,
                false,
                0,
            ),
            endpoint,
            attestation_header: ATTESTATION_UNAVAILABLE.to_owned(),
            session_id: None,
            version: RealtimeVersion::V3,
        }
    }
}

pub(super) struct WebRtcMedia {
    peer: Arc<RTCPeerConnection>,
    input: mpsc::Sender<RealtimeAudio>,
    audio: mpsc::Receiver<Result<RealtimeAudio, RealtimeError>>,
}

impl WebRtcMedia {
    pub(super) fn input(&self) -> mpsc::Sender<RealtimeAudio> {
        self.input.clone()
    }

    pub(super) async fn recv(&mut self) -> Option<Result<RealtimeAudio, RealtimeError>> {
        self.audio.recv().await
    }

    pub(super) async fn close(self) {
        if let Err(error) = self.peer.close().await {
            debug!(%error, "failed to close GPT Realtime WebRTC peer");
        }
    }
}

pub(super) struct ConnectConfig<'a> {
    pub(super) auth: &'a OpenAiAuth,
    pub(super) api_base_url: &'a str,
    pub(super) attestation_header: Option<&'a str>,
    pub(super) websocket_url: Option<&'a str>,
    pub(super) instructions: &'a str,
    pub(super) model: &'a str,
    pub(super) voice: RealtimeVoice,
    pub(super) session_id: Option<&'a str>,
    pub(super) initial_items: &'a [RealtimeInitialItem],
    pub(super) delegation_ack_filler: Option<bool>,
    pub(super) version: RealtimeVersion,
}

pub(super) struct ExistingCallConfig<'a> {
    pub(super) auth: &'a OpenAiAuth,
    pub(super) attestation_header: Option<&'a str>,
    pub(super) websocket_url: Option<&'a str>,
    pub(super) session_id: Option<&'a str>,
    pub(super) version: RealtimeVersion,
    pub(super) call_id: &'a str,
}

pub(super) async fn connect(config: ConnectConfig<'_>) -> Result<WebRtcConnection, RealtimeError> {
    let offer = create_offer().await?;
    let (call, auth) = create_call_with_auth_recovery(&config, &offer.sdp).await?;
    trace!(target: "nanocodex_oai_api::realtime::wire", sdp = %call.sdp, call_id = %call.id, "GPT Realtime WebRTC answer");
    debug!(call_id = %call.id, "applying GPT Realtime WebRTC answer");
    timeout(
        CONNECT_TIMEOUT,
        offer.peer.set_remote_description(
            RTCSessionDescription::answer(call.sdp)
                .map_err(|error| RealtimeError::WebRtc(error.to_string()))?,
        ),
    )
    .await
    .map_err(|_| RealtimeError::ConnectTimeout)?
    .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;
    debug!(call_id = %call.id, "applied GPT Realtime WebRTC answer");

    let sideband = WebRtcSideband::from_config(&config, auth, &call.id)?;
    let (mut socket, response) = connect_sideband(&sideband).await?;
    debug!(
        status = response.status().as_u16(),
        "connected GPT Realtime WebRTC sideband"
    );
    if config.version == RealtimeVersion::V1 {
        send_v1_session_update(&mut socket, &config).await?;
    }
    Ok(WebRtcConnection {
        socket,
        sideband,
        media: WebRtcMedia {
            peer: offer.peer,
            input: offer.input,
            audio: offer.audio,
        },
    })
}

async fn send_v1_session_update(
    socket: &mut Socket,
    config: &ConnectConfig<'_>,
) -> Result<(), RealtimeError> {
    super::send_json(socket, &v1_session_update(config)).await
}

fn v1_session_update(config: &ConnectConfig<'_>) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "quicksilver",
            "instructions": config.instructions,
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": super::REALTIME_SAMPLE_RATE }
                },
                "output": { "voice": config.voice.as_str() }
            }
        }
    })
}

pub(super) async fn prepare_with_sdp(
    config: ConnectConfig<'_>,
    sdp: &str,
) -> Result<PreparedCallerSdpCall, RealtimeError> {
    let (call, auth) = create_call_with_auth_recovery(&config, sdp).await?;
    trace!(target: "nanocodex_oai_api::realtime::wire", sdp = %call.sdp, call_id = %call.id, "GPT Realtime WebRTC answer");
    let sideband = WebRtcSideband::from_config(&config, auth, &call.id)?;
    let initial_update =
        (config.version == RealtimeVersion::V1).then(|| v1_session_update(&config));
    Ok(PreparedCallerSdpCall {
        sdp: call.sdp,
        sideband,
        initial_update,
    })
}

pub(super) async fn connect_existing_call(
    config: ExistingCallConfig<'_>,
) -> Result<ExistingCallConnection, RealtimeError> {
    let auth = config.auth.snapshot().await?;
    let sideband = WebRtcSideband {
        auth,
        endpoint: sideband_endpoint(config.websocket_url, config.call_id, config.version)?,
        attestation_header: config
            .attestation_header
            .unwrap_or(ATTESTATION_UNAVAILABLE)
            .to_owned(),
        session_id: config.session_id.map(str::to_owned),
        version: config.version,
    };
    let (socket, response) = connect_sideband(&sideband).await?;
    debug!(
        status = response.status().as_u16(),
        "attached GPT Realtime existing-call sideband"
    );
    Ok(ExistingCallConnection { socket, sideband })
}

async fn connect_sideband(
    sideband: &WebRtcSideband,
) -> Result<
    (
        Socket,
        tokio_tungstenite::tungstenite::http::Response<Option<Vec<u8>>>,
    ),
    RealtimeError,
> {
    debug!(url = %sideband.endpoint, "connecting GPT Realtime WebRTC sideband");
    let mut last_error = None;
    for attempt in 0..=3_u32 {
        let connect_started = tokio::time::Instant::now();
        let request = sideband_request(sideband)?;
        match timeout(CONNECT_TIMEOUT, connect_async(request)).await {
            Ok(Ok((socket, response))) => {
                debug!(
                    attempt,
                    elapsed_ms = connect_started.elapsed().as_millis(),
                    "joined GPT Realtime WebRTC sideband"
                );
                return Ok((socket, response));
            }
            Ok(Err(error)) => {
                let error = map_sideband_websocket_error(error);
                if sideband_session_ended(&error) {
                    return Err(error);
                }
                last_error = Some(error);
            }
            Err(_) => last_error = Some(RealtimeError::ConnectTimeout),
        }
        if attempt < 3 {
            let delay = Duration::from_millis(100_u64.saturating_mul(1_u64 << attempt));
            warn!(
                attempt,
                delay_ms = delay.as_millis(),
                "retrying GPT Realtime sideband join"
            );
            sleep(delay).await;
        }
    }
    Err(last_error.unwrap_or(RealtimeError::ConnectTimeout))
}

fn sideband_request(
    sideband: &WebRtcSideband,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, RealtimeError> {
    let mut request = sideband
        .endpoint
        .as_str()
        .into_client_request()
        .map_err(|error| RealtimeError::InvalidUrl(error.to_string()))?;
    add_auth_headers(request.headers_mut(), &sideband.auth)?;
    request.headers_mut().insert(
        "x-oai-attestation",
        HeaderValue::from_str(&sideband.attestation_header)
            .map_err(|error| RealtimeError::InvalidAuthorization(error.to_string()))?,
    );
    request.headers_mut().insert(
        "openai-alpha",
        HeaderValue::from_static(alpha_header(sideband.version)),
    );
    request.headers_mut().insert(
        header::USER_AGENT,
        HeaderValue::from_static(concat!("nanocodex/", env!("CARGO_PKG_VERSION"))),
    );
    request
        .headers_mut()
        .insert("originator", HeaderValue::from_static("nanocodex"));
    if let Some(session_id) = sideband.session_id.as_deref() {
        let value = HeaderValue::from_str(session_id)
            .map_err(|error| RealtimeError::InvalidSessionId(error.to_string()))?;
        request.headers_mut().insert("x-session-id", value.clone());
        request.headers_mut().insert("session-id", value.clone());
        request.headers_mut().insert("thread-id", value);
    }

    Ok(request)
}

fn map_sideband_websocket_error(error: tokio_tungstenite::tungstenite::Error) -> RealtimeError {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            RealtimeError::WebSocketHandshake {
                status: response.status().as_u16(),
            }
        }
        error => map_websocket_error(error),
    }
}

pub(super) const fn sideband_session_ended(error: &RealtimeError) -> bool {
    matches!(
        error,
        RealtimeError::WebSocketHandshake { status: 404 | 410 }
    )
}

async fn create_offer() -> Result<Offer, RealtimeError> {
    let mut media_engine = MediaEngine::default();
    media_engine
        .register_codec(
            RTCRtpCodecParameters {
                capability: RTCRtpCodecCapability {
                    mime_type: MIME_TYPE_OPUS.to_owned(),
                    clock_rate: 48_000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                    rtcp_feedback: Vec::new(),
                },
                payload_type: 111,
                ..Default::default()
            },
            RTPCodecType::Audio,
        )
        .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;
    let registry = register_default_interceptors(Registry::new(), &mut media_engine)
        .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;
    let api = APIBuilder::new()
        .with_media_engine(media_engine)
        .with_interceptor_registry(registry)
        .build();
    let peer = Arc::new(
        api.new_peer_connection(RTCConfiguration::default())
            .await
            .map_err(|error| RealtimeError::WebRtc(error.to_string()))?,
    );
    let microphone = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_OPUS.to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: Vec::new(),
        },
        "audio".to_owned(),
        "nanocodex".to_owned(),
    ));
    let sender = peer
        .add_track(Arc::clone(&microphone) as Arc<dyn TrackLocal + Send + Sync>)
        .await
        .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;
    tokio::spawn(async move {
        let mut buffer = vec![0_u8; 1_500];
        while sender.read(&mut buffer).await.is_ok() {}
    });
    peer.create_data_channel(DATA_CHANNEL_LABEL, None)
        .await
        .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;

    let (audio_tx, audio_rx) = mpsc::channel(EVENT_CAPACITY);
    let (input_tx, input_rx) = mpsc::channel(EVENT_CAPACITY);
    spawn_microphone_encoder(input_rx, microphone, audio_tx.clone())?;
    peer.on_track(Box::new(move |track, _, _| {
        let audio_tx = audio_tx.clone();
        Box::pin(async move {
            let codec = track.codec();
            if !codec
                .capability
                .mime_type
                .eq_ignore_ascii_case(MIME_TYPE_OPUS)
            {
                let _ = audio_tx
                    .send(Err(RealtimeError::WebRtc(format!(
                        "unsupported inbound audio codec {}",
                        codec.capability.mime_type
                    ))))
                    .await;
                return;
            }
            let mut decoder = match OpusDecoder::new(OpusChannels::Mono, OpusSampleRate::Hz24000) {
                Ok(decoder) => decoder,
                Err(error) => {
                    let _ = audio_tx
                        .send(Err(RealtimeError::WebRtc(format!(
                            "failed to initialize Opus decoder: {}",
                            error.message()
                        ))))
                        .await;
                    return;
                }
            };
            let mut samples = inbound_sample_builder();
            let mut pcm = vec![0_u16; WEBRTC_MAX_FRAME_SAMPLES];
            loop {
                let packet = match track.read_rtp().await {
                    Ok((packet, _)) => packet,
                    Err(error) => {
                        debug!(%error, "GPT Realtime WebRTC audio track stopped");
                        return;
                    }
                };
                samples.push(packet);
                while let Some(sample) = samples.pop() {
                    let dropped = sample
                        .prev_dropped_packets
                        .saturating_sub(sample.prev_padding_packets)
                        .min(WEBRTC_MAX_CONCEALED_PACKETS);
                    let frame_samples = match decoder.get_nb_samples(&sample.data) {
                        Ok(samples) if samples <= WEBRTC_MAX_FRAME_SAMPLES => samples,
                        Ok(samples) => {
                            let _ = audio_tx
                                .send(Err(RealtimeError::WebRtc(format!(
                                    "Realtime Opus packet declared an oversized {samples}-sample frame"
                                ))))
                                .await;
                            return;
                        }
                        Err(error) => {
                            let _ = audio_tx
                                .send(Err(RealtimeError::WebRtc(format!(
                                    "failed to inspect Realtime Opus audio: {}",
                                    error.message()
                                ))))
                                .await;
                            return;
                        }
                    };
                    for missing in 0..dropped {
                        let recover_with_fec = missing + 1 == dropped;
                        let packet = if recover_with_fec {
                            sample.data.as_ref()
                        } else {
                            &[]
                        };
                        let concealed = match decoder.decode_to_slice(
                            packet,
                            &mut pcm[..frame_samples],
                            recover_with_fec,
                        ) {
                            Ok(concealed) => concealed,
                            Err(error) => {
                                let _ = audio_tx
                                    .send(Err(RealtimeError::WebRtc(format!(
                                        "failed to conceal dropped Realtime Opus audio: {}",
                                        error.message()
                                    ))))
                                    .await;
                                return;
                            }
                        };
                        if send_decoded_audio(&audio_tx, &pcm[..concealed])
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    let decoded = match decoder.decode_to_slice(&sample.data, &mut pcm, false) {
                        Ok(decoded) => decoded,
                        Err(error) => {
                            let _ = audio_tx
                                .send(Err(RealtimeError::WebRtc(format!(
                                    "failed to decode Realtime Opus audio: {}",
                                    error.message()
                                ))))
                                .await;
                            return;
                        }
                    };
                    if send_decoded_audio(&audio_tx, &pcm[..decoded])
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        })
    }));

    let offer = peer
        .create_offer(None)
        .await
        .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;
    let mut gathering = peer.gathering_complete_promise().await;
    peer.set_local_description(offer)
        .await
        .map_err(|error| RealtimeError::WebRtc(error.to_string()))?;
    timeout(CONNECT_TIMEOUT, gathering.recv())
        .await
        .map_err(|_| RealtimeError::ConnectTimeout)?;
    let offer = peer
        .local_description()
        .await
        .ok_or_else(|| RealtimeError::WebRtc("WebRTC offer was not retained".to_owned()))?;
    trace!(target: "nanocodex_oai_api::realtime::wire", sdp = %offer.sdp, "GPT Realtime WebRTC offer");
    Ok(Offer {
        peer,
        sdp: offer.sdp,
        input: input_tx,
        audio: audio_rx,
    })
}

fn inbound_sample_builder() -> SampleBuilder<OpusPacket> {
    SampleBuilder::new(WEBRTC_REORDER_PACKETS, OpusPacket, 48_000)
        .with_max_time_delay(WEBRTC_REORDER_DELAY)
}

async fn send_decoded_audio(
    audio: &mpsc::Sender<Result<RealtimeAudio, RealtimeError>>,
    samples: &[u16],
) -> Result<(), ()> {
    audio
        .send(Ok(RealtimeAudio::from_samples(
            samples.iter().map(|sample| *sample as i16),
        )))
        .await
        .map_err(|_| ())
}

struct Offer {
    peer: Arc<RTCPeerConnection>,
    sdp: String,
    input: mpsc::Sender<RealtimeAudio>,
    audio: mpsc::Receiver<Result<RealtimeAudio, RealtimeError>>,
}

fn spawn_microphone_encoder(
    mut input: mpsc::Receiver<RealtimeAudio>,
    track: Arc<TrackLocalStaticSample>,
    errors: mpsc::Sender<Result<RealtimeAudio, RealtimeError>>,
) -> Result<(), RealtimeError> {
    let mut encoder = OpusEncoder::new(
        OpusChannels::Mono,
        OpusSampleRate::Hz24000,
        OpusApplication::Voip,
    )
    .map_err(|error| {
        RealtimeError::WebRtc(format!(
            "failed to initialize Opus encoder: {}",
            error.message()
        ))
    })?;
    encoder
        .set_signal(OpusSignal::Voice)
        .and_then(|()| encoder.set_inband_fec(OpusInbandFec::Mode1))
        .and_then(|()| encoder.set_packet_loss(WEBRTC_EXPECTED_PACKET_LOSS_PERCENT))
        .map_err(|error| {
            RealtimeError::WebRtc(format!(
                "failed to configure microphone Opus encoder: {}",
                error.message()
            ))
        })?;
    tokio::spawn(async move {
        let mut pending = VecDeque::with_capacity(WEBRTC_INPUT_FRAME_SAMPLES * 2);
        let mut pcm = Vec::with_capacity(WEBRTC_INPUT_FRAME_SAMPLES);
        let mut encoded = vec![0_u8; OPUS_PACKET_CAPACITY];
        while let Some(audio) = input.recv().await {
            let (samples, _) = audio.as_bytes().as_chunks::<2>();
            pending.extend(samples.iter().map(|sample| {
                f32::from(i16::from_le_bytes([sample[0], sample[1]])) / f32::from(i16::MAX)
            }));
            while pending.len() >= WEBRTC_INPUT_FRAME_SAMPLES {
                pcm.extend(pending.drain(..WEBRTC_INPUT_FRAME_SAMPLES));
                let encoded_bytes = match encoder.encode_float_to_slice(&pcm, &mut encoded) {
                    Ok(encoded_bytes) => encoded_bytes,
                    Err(error) => {
                        let _ = errors
                            .send(Err(RealtimeError::WebRtc(format!(
                                "failed to encode microphone audio: {}",
                                error.message()
                            ))))
                            .await;
                        return;
                    }
                };
                pcm.clear();
                if let Err(error) = track
                    .write_sample(&Sample {
                        data: encoded[..encoded_bytes].to_vec().into(),
                        duration: Duration::from_millis(20),
                        ..Default::default()
                    })
                    .await
                {
                    let _ = errors
                        .send(Err(RealtimeError::WebRtc(format!(
                            "failed to send microphone audio: {error}"
                        ))))
                        .await;
                    return;
                }
            }
        }
    });
    Ok(())
}

#[derive(Serialize)]
struct CallRequest<'a> {
    sdp: &'a str,
    session: Value,
}

struct CallResponse {
    sdp: String,
    id: String,
}

fn call_session(config: &ConnectConfig<'_>) -> Value {
    match config.version {
        RealtimeVersion::V1 => json!({
            "type": "quicksilver",
            "model": config.model,
            "instructions": config.instructions,
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": super::REALTIME_SAMPLE_RATE }
                },
                "output": { "voice": config.voice.as_str() }
            }
        }),
        RealtimeVersion::V2 => unreachable!("realtime v2 does not support AVAS WebRTC"),
        RealtimeVersion::V3 => {
            let initial_items = config
                .initial_items
                .iter()
                .map(|item| {
                    json!({
                        "type": "message",
                        "role": item.role.as_str(),
                        "content": [{
                            "type": item.role.content_type(),
                            "text": item.text,
                        }],
                    })
                })
                .collect::<Vec<_>>();
            let mut session = json!({
                "model": config.model,
                "instructions": config.instructions,
                "audio": { "output": { "voice": config.voice.as_str() } },
                "delegation": { "type": "client" },
            });
            if let Some(delegation_ack_filler) = config.delegation_ack_filler {
                session["delegation"]["ack_filler"] = Value::Bool(delegation_ack_filler);
            }
            if !initial_items.is_empty() {
                session["initial_items"] = Value::Array(initial_items);
            }
            session
        }
    }
}

const fn alpha_header(version: RealtimeVersion) -> &'static str {
    match version {
        RealtimeVersion::V1 => "quicksilver=v1",
        RealtimeVersion::V2 => "",
        RealtimeVersion::V3 => "quicksilver=v2",
    }
}

async fn create_call_with_auth_recovery(
    config: &ConnectConfig<'_>,
    offer: &str,
) -> Result<(CallResponse, OpenAiAuthSnapshot), RealtimeError> {
    let auth = config.auth.snapshot().await?;
    match create_call(config, offer, &auth).await {
        Err(CallAttemptError::Unauthorized) => {
            config.auth.recover_unauthorized(&auth).await?;
            let refreshed = config.auth.snapshot().await?;
            let call = create_call(config, offer, &refreshed)
                .await
                .map_err(CallAttemptError::into_realtime)?;
            Ok((call, refreshed))
        }
        Ok(call) => Ok((call, auth)),
        Err(error) => Err(error.into_realtime()),
    }
}

enum CallAttemptError {
    Unauthorized,
    Other(RealtimeError),
}

impl CallAttemptError {
    fn into_realtime(self) -> RealtimeError {
        match self {
            Self::Unauthorized => {
                RealtimeError::Http("ChatGPT rejected refreshed Realtime authorization".to_owned())
            }
            Self::Other(error) => error,
        }
    }
}

async fn create_call(
    config: &ConnectConfig<'_>,
    offer: &str,
    auth: &OpenAiAuthSnapshot,
) -> Result<CallResponse, CallAttemptError> {
    let backend_request = uses_backend_request_shape(config.api_base_url);
    let endpoint =
        call_endpoint(config.api_base_url, config.version).map_err(CallAttemptError::Other)?;
    let request = CallRequest {
        sdp: offer,
        session: call_session(config),
    };
    let mut builder = realtime_http_client()
        .post(endpoint)
        .bearer_auth(auth.bearer())
        .header("openai-alpha", alpha_header(config.version))
        .header("originator", "nanocodex")
        .header(
            "x-oai-attestation",
            config.attestation_header.unwrap_or(ATTESTATION_UNAVAILABLE),
        );
    if backend_request {
        builder = builder.json(&request);
    } else {
        builder = builder
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={MULTIPART_BOUNDARY}"),
            )
            .body(multipart_call_body(offer, &request.session)?);
    }
    if let Some(account_id) = auth.account_id() {
        builder = builder.header("ChatGPT-Account-ID", account_id);
    }
    if let Some(session_id) = config.session_id {
        builder = builder
            .header("x-session-id", session_id)
            .header("session-id", session_id)
            .header("thread-id", session_id);
    }
    if tracing::enabled!(target: "nanocodex_oai_api::realtime::wire", tracing::Level::TRACE) {
        let payload = serde_json::to_string(&request)
            .map_err(|error| CallAttemptError::Other(RealtimeError::Message(error.to_string())))?;
        trace!(target: "nanocodex_oai_api::realtime::wire", payload = %payload, "GPT Realtime call request");
    }
    let response = timeout(CONNECT_TIMEOUT, builder.send())
        .await
        .map_err(|_| CallAttemptError::Other(RealtimeError::ConnectTimeout))?
        .map_err(|error| CallAttemptError::Other(RealtimeError::Http(error.to_string())))?;
    if response.status() == StatusCode::UNAUTHORIZED {
        return Err(CallAttemptError::Unauthorized);
    }
    let status = response.status();
    let location = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let limit = if status.is_success() {
        CALL_BODY_LIMIT
    } else {
        ERROR_BODY_LIMIT
    };
    let body = bounded_body(response, limit)
        .await
        .map_err(CallAttemptError::Other)?;
    if !status.is_success() {
        return Err(CallAttemptError::Other(RealtimeError::Http(format!(
            "Realtime call creation returned {status}: {}",
            String::from_utf8_lossy(&body)
        ))));
    }
    let sdp = String::from_utf8(body).map_err(|error| {
        CallAttemptError::Other(RealtimeError::Http(format!(
            "Realtime call returned invalid SDP: {error}"
        )))
    })?;
    let id = call_id(location.as_deref()).map_err(CallAttemptError::Other)?;
    Ok(CallResponse { sdp, id })
}

fn multipart_call_body(offer: &str, session: &Value) -> Result<Vec<u8>, CallAttemptError> {
    let session = serde_json::to_string(session)
        .map_err(|error| CallAttemptError::Other(RealtimeError::Message(error.to_string())))?;
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"sdp\"\r\n");
    body.extend_from_slice(b"Content-Type: application/sdp\r\n\r\n");
    body.extend_from_slice(offer.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"session\"\r\n");
    body.extend_from_slice(b"Content-Type: application/json\r\n\r\n");
    body.extend_from_slice(session.as_bytes());
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{MULTIPART_BOUNDARY}--\r\n").as_bytes());
    Ok(body)
}

fn realtime_http_client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(Client::new).clone()
}

async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, RealtimeError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| RealtimeError::Http(error.to_string()))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(RealtimeError::Http(format!(
                "Realtime call response exceeded {limit} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn add_auth_headers(
    headers: &mut http::HeaderMap,
    auth: &OpenAiAuthSnapshot,
) -> Result<(), RealtimeError> {
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", auth.bearer()))
            .map_err(|error| RealtimeError::InvalidAuthorization(error.to_string()))?,
    );
    if let Some(account_id) = auth.account_id() {
        headers.insert(
            "ChatGPT-Account-ID",
            HeaderValue::from_str(account_id)
                .map_err(|error| RealtimeError::InvalidAuthorization(error.to_string()))?,
        );
    }
    Ok(())
}

fn uses_backend_request_shape(api_base_url: &str) -> bool {
    api_base_url.contains("/backend-api")
}

fn call_endpoint(api_base_url: &str, version: RealtimeVersion) -> Result<Url, RealtimeError> {
    let mut endpoint =
        Url::parse(api_base_url).map_err(|error| RealtimeError::InvalidUrl(error.to_string()))?;
    let path = endpoint.path().trim_end_matches('/');
    let backend_request = uses_backend_request_shape(api_base_url);
    if version == RealtimeVersion::V3 && !backend_request {
        endpoint.set_path(&format!("{path}/live"));
    } else {
        endpoint.set_path(&format!("{path}/realtime/calls"));
    }
    if version == RealtimeVersion::V1 || (backend_request && version == RealtimeVersion::V3) {
        endpoint
            .query_pairs_mut()
            .append_pair("intent", "quicksilver")
            .append_pair("architecture", "avas");
    }
    Ok(endpoint)
}

fn sideband_endpoint(
    explicit: Option<&str>,
    call_id: &str,
    version: RealtimeVersion,
) -> Result<Url, RealtimeError> {
    let base = explicit.unwrap_or(OPENAI_REALTIME_BASE);
    let mut endpoint =
        Url::parse(base).map_err(|error| RealtimeError::InvalidUrl(error.to_string()))?;
    match endpoint.scheme() {
        "https" => endpoint
            .set_scheme("wss")
            .map_err(|()| RealtimeError::InvalidUrl("could not select wss".to_owned()))?,
        "http" => endpoint
            .set_scheme("ws")
            .map_err(|()| RealtimeError::InvalidUrl("could not select ws".to_owned()))?,
        "wss" | "ws" => {}
        scheme => {
            return Err(RealtimeError::InvalidUrl(format!(
                "unsupported URL scheme {scheme}"
            )));
        }
    }
    if version == RealtimeVersion::V3 {
        if matches!(call_id, "." | "..") {
            return Err(RealtimeError::InvalidConfiguration(format!(
                "invalid realtime call id: {call_id}"
            )));
        }
        let path = endpoint.path().to_owned();
        if path.is_empty() || path == "/" || path == "/v1" || path == "/v1/" {
            endpoint.set_path("/v1/live");
        } else if let Some(prefix) = path.trim_end_matches('/').strip_suffix("/realtime") {
            endpoint.set_path(&format!("{prefix}/live"));
        } else if path.ends_with("/live/") {
            endpoint.set_path(path.trim_end_matches('/'));
        }
        endpoint
            .path_segments_mut()
            .map_err(|()| {
                RealtimeError::InvalidUrl(
                    "realtime sideband URL cannot contain path segments".to_owned(),
                )
            })?
            .pop_if_empty()
            .push(call_id);
    } else {
        let path = endpoint.path().to_owned();
        if path.is_empty() || path == "/" {
            endpoint.set_path("/v1/realtime");
        } else if path.ends_with("/v1") {
            endpoint.set_path(&format!("{path}/realtime"));
        } else if path.ends_with("/v1/") {
            endpoint.set_path(&format!("{path}realtime"));
        }
        let mut query = endpoint.query_pairs_mut();
        if version == RealtimeVersion::V1 {
            query.append_pair("intent", "quicksilver");
        }
        query.append_pair("call_id", call_id);
    }
    Ok(endpoint)
}

fn call_id(location: Option<&str>) -> Result<String, RealtimeError> {
    let location = location
        .ok_or_else(|| RealtimeError::Http("Realtime call response omitted Location".to_owned()))?;
    location
        .split('?')
        .next()
        .unwrap_or(location)
        .rsplit('/')
        .find(|segment| (segment.starts_with("rtc_") && segment.len() > 4) || is_uuid(segment))
        .map(str::to_owned)
        .ok_or_else(|| RealtimeError::Http("Realtime call Location omitted a call ID".to_owned()))
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, character)| match index {
            8 | 13 | 18 | 23 => character == '-',
            _ => character.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
mod tests {
    use webrtc::rtp::{header::Header, packet::Packet};

    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use super::{
        ConnectConfig, OPUS_PACKET_CAPACITY, OpusApplication, OpusChannels, OpusDecoder,
        OpusEncoder, OpusSampleRate, WEBRTC_INPUT_FRAME_SAMPLES, call_endpoint, call_id,
        create_call, inbound_sample_builder, sideband_endpoint, sideband_session_ended,
    };
    use crate::{
        OpenAiAuth, OpenAiAuthMode, OpenAiAuthSnapshot,
        realtime::{RealtimeInitialItem, RealtimeTextRole, RealtimeVersion, RealtimeVoice},
    };

    #[test]
    fn derives_chatgpt_call_and_direct_sideband_endpoints() {
        assert_eq!(
            call_endpoint("https://chatgpt.com/backend-api/codex", RealtimeVersion::V3,)
                .unwrap()
                .as_str(),
            "https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas"
        );
        assert_eq!(
            call_endpoint("https://api.openai.com/v1", RealtimeVersion::V3)
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/live"
        );
        assert_eq!(
            call_endpoint("https://api.openai.com/v1", RealtimeVersion::V1)
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/realtime/calls?intent=quicksilver&architecture=avas"
        );
        assert_eq!(
            sideband_endpoint(None, "rtc_test", RealtimeVersion::V3)
                .unwrap()
                .as_str(),
            "wss://api.openai.com/v1/live/rtc_test"
        );
        assert_eq!(
            sideband_endpoint(
                Some("wss://example.test/v1/realtime"),
                "rtc_test",
                RealtimeVersion::V3,
            )
            .unwrap()
            .as_str(),
            "wss://example.test/v1/live/rtc_test"
        );
        assert_eq!(
            sideband_endpoint(None, "rtc_test", RealtimeVersion::V1)
                .unwrap()
                .as_str(),
            "wss://api.openai.com/v1/realtime?intent=quicksilver&call_id=rtc_test"
        );
        assert_eq!(
            sideband_endpoint(None, "rtc/a", RealtimeVersion::V3)
                .unwrap()
                .as_str(),
            "wss://api.openai.com/v1/live/rtc%2Fa"
        );
        assert_eq!(
            sideband_endpoint(
                Some("wss://example.test/v1?tenant=one"),
                "rtc_test",
                RealtimeVersion::V3,
            )
            .unwrap()
            .as_str(),
            "wss://example.test/v1/live/rtc_test?tenant=one"
        );
        assert!(sideband_endpoint(None, ".", RealtimeVersion::V3).is_err());
    }

    #[test]
    fn parses_supported_call_location_shapes() {
        assert_eq!(
            call_id(Some("/v1/realtime/calls/rtc_test?foo=bar")).unwrap(),
            "rtc_test"
        );
        assert_eq!(
            call_id(Some(
                "https://api.openai.com/v1/realtime/calls/019eb97d-8e9a-7ff3-94b0-ea019babd5d7"
            ))
            .unwrap(),
            "019eb97d-8e9a-7ff3-94b0-ea019babd5d7"
        );
    }

    #[test]
    fn recognizes_terminal_sideband_statuses() {
        for status in [404, 410] {
            assert!(sideband_session_ended(
                &crate::realtime::RealtimeError::WebSocketHandshake { status }
            ));
        }
        assert!(!sideband_session_ended(
            &crate::realtime::RealtimeError::WebSocketHandshake { status: 500 }
        ));
    }

    #[test]
    fn encodes_one_realtime_pcm_frame_as_valid_opus() {
        let mut encoder = OpusEncoder::new(
            OpusChannels::Mono,
            OpusSampleRate::Hz24000,
            OpusApplication::Voip,
        )
        .unwrap();
        let mut packet = vec![0_u8; OPUS_PACKET_CAPACITY];
        let encoded = encoder
            .encode_float_to_slice(&[0.0; WEBRTC_INPUT_FRAME_SAMPLES], &mut packet)
            .unwrap();
        packet.truncate(encoded);

        let mut decoder = OpusDecoder::new(OpusChannels::Mono, OpusSampleRate::Hz24000).unwrap();
        let mut decoded = vec![0_u16; WEBRTC_INPUT_FRAME_SAMPLES];
        assert_eq!(
            decoder
                .decode_to_slice(&packet, &mut decoded, false)
                .unwrap(),
            WEBRTC_INPUT_FRAME_SAMPLES
        );
    }

    #[test]
    fn reorders_inbound_opus_packets_before_playback() {
        fn packet(sequence_number: u16, timestamp: u32) -> Packet {
            Packet {
                header: Header {
                    sequence_number,
                    timestamp,
                    marker: true,
                    ..Default::default()
                },
                payload: vec![sequence_number as u8].into(),
            }
        }

        let mut samples = inbound_sample_builder();
        samples.push(packet(10, 0));
        samples.push(packet(12, 1_920));
        assert!(samples.pop().is_none());

        samples.push(packet(11, 960));
        samples.push(packet(13, 2_880));

        assert_eq!(samples.pop().unwrap().data.as_ref(), &[10]);
        assert_eq!(samples.pop().unwrap().data.as_ref(), &[11]);
        assert_eq!(samples.pop().unwrap().data.as_ref(), &[12]);
    }

    #[tokio::test]
    async fn chatgpt_call_uses_account_auth_and_frameless_shape() {
        crate::transport::install_default_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0, "request ended before its JSON body");
                request.extend_from_slice(&chunk[..read]);
                let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.contains("authorization: Bearer chatgpt-token\r\n"));
            assert!(request.contains("chatgpt-account-id: account-1\r\n"));
            assert!(request.contains("x-session-id: session-1\r\n"));
            assert!(request.contains("session-id: session-1\r\n"));
            assert!(request.contains("thread-id: session-1\r\n"));
            assert!(request.contains("openai-alpha: quicksilver=v2\r\n"));
            assert!(request.contains("x-oai-attestation: {\"v\":1,\"s\":1}\r\n"));
            let body = request.split_once("\r\n\r\n").unwrap().1;
            let body: serde_json::Value = serde_json::from_str(body).unwrap();
            assert_eq!(body["sdp"], "v=offer\r\n");
            assert_eq!(body["session"]["model"], "gpt-live-1-codex");
            assert_eq!(body["session"]["audio"]["output"]["voice"], "cove");
            assert_eq!(body["session"]["delegation"]["type"], "client");
            assert_eq!(body["session"]["delegation"]["ack_filler"], false);
            assert_eq!(
                body["session"]["initial_items"],
                serde_json::json!([
                    {
                        "type": "message",
                        "role": "developer",
                        "content": [{"type": "input_text", "text": "Remember this."}],
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Understood."}],
                    },
                ])
            );
            assert!(body["session"].get("type").is_none());
            assert!(body["session"]["audio"].get("input").is_none());
            stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 10\r\nLocation: /v1/live/rtc_test\r\n\r\nv=answer\r\n",
                )
                .await
                .unwrap();
        });

        let auth_source = OpenAiAuth::api_key("unused");
        let api_base_url = format!("http://{address}/backend-api/codex");
        let initial_items = [
            RealtimeInitialItem::new(RealtimeTextRole::Developer, "Remember this."),
            RealtimeInitialItem::new(RealtimeTextRole::Assistant, "Understood."),
        ];
        let config = ConnectConfig {
            auth: &auth_source,
            api_base_url: &api_base_url,
            attestation_header: None,
            websocket_url: None,
            instructions: "delegate coding work",
            model: "gpt-live-1-codex",
            voice: RealtimeVoice::Cove,
            session_id: Some("session-1"),
            initial_items: &initial_items,
            delegation_ack_filler: Some(false),
            version: RealtimeVersion::V3,
        };
        let snapshot = OpenAiAuthSnapshot::new(
            OpenAiAuthMode::ChatGpt,
            "chatgpt-token",
            Some("account-1"),
            false,
            0,
        );
        let response = create_call(&config, "v=offer\r\n", &snapshot)
            .await
            .map_err(|error| error.into_realtime())
            .unwrap();
        assert_eq!(response.sdp, "v=answer\r\n");
        assert_eq!(response.id, "rtc_test");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn direct_v3_call_uses_live_multipart_shape() {
        crate::transport::install_default_rustls_crypto_provider();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let read = stream.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0, "request ended before its multipart body");
                request.extend_from_slice(&chunk[..read]);
                let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .unwrap();
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("POST /v1/live HTTP/1.1\r\n"));
            assert!(!request.contains("intent=quicksilver"));
            assert!(request.contains("authorization: Bearer api-token\r\n"));
            assert!(request.contains(
                "content-type: multipart/form-data; boundary=----nanocodex-realtime-boundary\r\n"
            ));
            let body = request.split_once("\r\n\r\n").unwrap().1;
            assert!(body.contains("Content-Disposition: form-data; name=\"sdp\""));
            assert!(body.contains("Content-Type: application/sdp\r\n\r\nv=offer\r\n"));
            assert!(body.contains("Content-Disposition: form-data; name=\"session\""));
            assert!(body.contains("\"model\":\"gpt-live-1-codex\""));
            assert!(body.contains("\"ack_filler\":true"));
            stream
                .write_all(
                    b"HTTP/1.1 201 Created\r\nContent-Length: 10\r\nLocation: /v1/live/rtc_direct\r\n\r\nv=answer\r\n",
                )
                .await
                .unwrap();
        });

        let auth_source = OpenAiAuth::api_key("api-token");
        let api_base_url = format!("http://{address}/v1");
        let config = ConnectConfig {
            auth: &auth_source,
            api_base_url: &api_base_url,
            attestation_header: None,
            websocket_url: None,
            instructions: "delegate coding work",
            model: "gpt-live-1-codex",
            voice: RealtimeVoice::Cove,
            session_id: None,
            initial_items: &[],
            delegation_ack_filler: Some(true),
            version: RealtimeVersion::V3,
        };
        let snapshot = OpenAiAuthSnapshot::new(
            OpenAiAuthMode::ApiKey,
            "api-token",
            Option::<String>::None,
            false,
            0,
        );
        let response = create_call(&config, "v=offer\r\n", &snapshot)
            .await
            .map_err(|error| error.into_realtime())
            .unwrap();
        assert_eq!(response.sdp, "v=answer\r\n");
        assert_eq!(response.id, "rtc_direct");
        server.await.unwrap();
    }
}
