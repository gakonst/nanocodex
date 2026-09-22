use super::*;
use std::{sync::Mutex, time::Duration};
use webrtc::{
    api::{
        API, APIBuilder, interceptor_registry::register_default_interceptors,
        setting_engine::SettingEngine,
    },
    interceptor::stream_info::RTPHeaderExtension,
    media::Sample,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
    rtp::{extension::playout_delay_extension::PlayoutDelayExtension, header::Header},
    rtp_transceiver::rtp_codec::RTCRtpCodecCapability,
    track::track_local::track_local_static_sample::TrackLocalStaticSample,
    util::marshal::{Marshal, Unmarshal},
};

#[derive(Default)]
struct Recorded(Mutex<Option<(Packet, Attributes)>>);
#[async_trait]
impl RTPWriter for Recorded {
    async fn write(&self, packet: &Packet, attributes: &Attributes) -> InterceptorResult<usize> {
        *self.0.lock().unwrap() = Some((packet.clone(), attributes.clone()));
        Ok(packet.payload.len())
    }
}

#[tokio::test]
async fn negotiated_video_hint_preserves_packets_without_leaking_across_viewers() {
    let mut original = Packet {
        header: Header {
            version: 2,
            sequence_number: 123,
            timestamp: 9000,
            ssrc: 42,
            marker: true,
            ..Default::default()
        },
        payload: vec![0x65, 0x88, 0x84].into(),
    };
    original.header.set_extension(3, vec![42].into()).unwrap();
    let before = original.clone();
    let attributes = Attributes::from([(9, 42)]);
    for (mime, id, expected) in [
        ("video/H264", Some(7), true),
        ("VIDEO/VP8", Some(8), true),
        ("video/H264", None, false),
        ("audio/opus", Some(7), false),
        ("video/H264", Some(0), false),
        ("video/H264", Some(-1), false),
        ("video/H264", Some(256), false),
    ] {
        let sink = Arc::new(Recorded::default());
        let info = StreamInfo {
            mime_type: mime.into(),
            rtp_header_extensions: id
                .into_iter()
                .map(|id| RTPHeaderExtension {
                    uri: URI.into(),
                    id,
                })
                .collect(),
            ..Default::default()
        };
        let writer = PlayoutDelay.bind_local_stream(&info, sink.clone()).await;
        assert_eq!(
            writer.write(&original, &attributes).await.unwrap(),
            original.payload.len()
        );
        let (mut received, actual_attributes) = sink.0.lock().unwrap().take().unwrap();
        assert_eq!(actual_attributes, attributes);
        if expected {
            let id = id.unwrap() as u8;
            let mut bytes = received.header.get_extension(id).unwrap();
            let delay = PlayoutDelayExtension::unmarshal(&mut bytes).unwrap();
            assert_eq!(
                delay,
                PlayoutDelayExtension {
                    min_delay: 0,
                    max_delay: 10
                }
            );
            assert_eq!(
                received.header.extensions.len(),
                before.header.extensions.len() + 1
            );
            received
                .header
                .extensions
                .retain(|extension| extension.id != id);
        }
        assert_eq!(received, before, "mime={mime} id={id:?}");
        assert_eq!(original, before, "shared packet was mutated");
    }
}

fn api(playout: bool) -> API {
    let mut engine = MediaEngine::default();
    engine.register_default_codecs().unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut engine).unwrap();
    let registry = if playout {
        register(&mut engine, registry).unwrap()
    } else {
        registry
    };
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    APIBuilder::new()
        .with_media_engine(engine)
        .with_interceptor_registry(registry)
        .with_setting_engine(settings)
        .build()
}

async fn gather(peer: &RTCPeerConnection, description: RTCSessionDescription) {
    let mut done = peer.gathering_complete_promise().await;
    peer.set_local_description(description).await.unwrap();
    done.recv().await;
}

async fn real_peer_roundtrip(supported: bool) {
    let publisher = api(true)
        .new_peer_connection(RTCConfiguration::default())
        .await
        .unwrap();
    let viewer = api(supported)
        .new_peer_connection(RTCConfiguration::default())
        .await
        .unwrap();
    let track = Arc::new(TrackLocalStaticSample::new(
        RTCRtpCodecCapability {
            mime_type: "video/H264".into(),
            clock_rate: 90_000,
            ..Default::default()
        },
        "screen".into(),
        "playout-test".into(),
    ));
    let sender = publisher.add_track(track.clone()).await.unwrap();
    let rtcp_sender = sender.clone();
    let rtcp = tokio::spawn(async move { while rtcp_sender.read_rtcp().await.is_ok() {} });
    let (received, mut incoming) = tokio::sync::mpsc::channel(1);
    viewer.on_track(Box::new(move |track, _, _| {
        let received = received.clone();
        Box::pin(async move {
            let _ = received.send(track.read_rtp().await).await;
        })
    }));
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let offer = publisher.create_offer(None).await.unwrap();
        assert!(offer.sdp.contains(URI));
        assert!(offer.sdp.contains("nack pli"), "default feedback must remain available");
        gather(&publisher, offer).await;
        viewer.set_remote_description(publisher.local_description().await.unwrap()).await.unwrap();
        let answer = viewer.create_answer(None).await.unwrap();
        assert_eq!(answer.sdp.contains(URI), supported);
        gather(&viewer, answer).await;
        publisher.set_remote_description(viewer.local_description().await.unwrap()).await.unwrap();
        let parameters = sender.get_parameters().await;
        let extension = parameters.rtp_parameters.header_extensions.iter().find(|extension| extension.uri == URI);
        assert_eq!(extension.is_some(), supported);
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        loop {
            tokio::select! {
                packet = incoming.recv() => {
                    let (packet, _) = packet.expect("track callback").expect("received RTP");
                    if let Some(extension) = extension {
                        let mut bytes = packet.header.get_extension(extension.id as u8).expect("negotiated playout hint");
                        assert_eq!(PlayoutDelayExtension::unmarshal(&mut bytes).unwrap(), PlayoutDelayExtension { min_delay: 0, max_delay: 10 });
                    } else {
                        assert!(packet.header.extensions.is_empty(), "legacy viewer received an unnegotiated extension");
                    }
                    break;
                }
                _ = tick.tick() => {
                    track.write_sample(&Sample { data: vec![0, 0, 0, 1, 0x65, 0x88, 0x84].into(), duration: Duration::from_millis(20), ..Default::default() }).await.unwrap();
                }
            }
        }
    }).await;
    publisher.close().await.unwrap();
    viewer.close().await.unwrap();
    rtcp.abort();
    result.expect("loopback media arrived within deadline");
}

#[tokio::test]
async fn negotiated_hint_arrives_over_real_peer_connection() {
    real_peer_roundtrip(true).await;
}

#[tokio::test]
async fn legacy_peer_receives_media_without_hint() {
    real_peer_roundtrip(false).await;
}

#[tokio::test]
async fn large_extension_ids_use_two_byte_wire_format_without_changing_source() {
    for id in [15, 255] {
        for existing_extension in [false, true] {
            let mut source = Packet {
                header: Header {
                    version: 2,
                    sequence_number: 91,
                    ..Default::default()
                },
                payload: vec![0x65, 0x88, 0x84].into(),
            };
            if existing_extension {
                source.header.set_extension(3, vec![42].into()).unwrap();
            }
            let original = source.clone();
            let sink = Arc::new(Recorded::default());
            let info = StreamInfo {
                mime_type: "video/H264".into(),
                rtp_header_extensions: vec![RTPHeaderExtension {
                    uri: URI.into(),
                    id,
                }],
                ..Default::default()
            };
            let writer = PlayoutDelay.bind_local_stream(&info, sink.clone()).await;
            writer.write(&source, &Attributes::new()).await.unwrap();
            let (output, _) = sink.0.lock().unwrap().take().unwrap();
            let mut bytes = output.marshal().unwrap();
            let parsed = Packet::unmarshal(&mut bytes).unwrap();
            assert_eq!(parsed.header.extension_profile, EXTENSION_PROFILE_TWO_BYTE);
            assert_eq!(
                parsed.header.get_extension(id as u8).unwrap().as_ref(),
                &[0, 0, 10]
            );
            if existing_extension {
                assert_eq!(parsed.header.get_extension(3).unwrap().as_ref(), &[42]);
            }
            assert_eq!(parsed.payload, original.payload);
            assert_eq!(parsed.header.sequence_number, 91);
            assert_eq!(source, original);
        }
    }
}

#[tokio::test]
async fn unsupported_header_profile_keeps_video_flowing_without_the_hint() {
    let original = Packet {
        header: Header {
            version: 2,
            extension: true,
            extension_profile: 0xABCD,
            extensions: vec![Extension {
                id: 0,
                payload: vec![1, 2, 3, 4].into(),
            }],
            ..Default::default()
        },
        payload: vec![0x65, 0x88, 0x84].into(),
    };
    let sink = Arc::new(Recorded::default());
    let info = StreamInfo {
        mime_type: "video/H264".into(),
        rtp_header_extensions: vec![RTPHeaderExtension {
            uri: URI.into(),
            id: 7,
        }],
        ..Default::default()
    };
    let writer = PlayoutDelay.bind_local_stream(&info, sink.clone()).await;
    writer.write(&original, &Attributes::new()).await.unwrap();
    assert_eq!(sink.0.lock().unwrap().take().unwrap().0, original);
}
