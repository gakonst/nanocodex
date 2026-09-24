use super::*;
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
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
                    max_delay: 3
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
    let mut registry = Registry::new();
    registry.add(Box::new(crate::media_guard::Guard(Arc::new(|outcome| {
        panic!("real peer media retired: {outcome}")
    }))));
    let registry = register_default_interceptors(registry, &mut engine).unwrap();
    let registry = if playout {
        register(&mut engine, registry).unwrap()
    } else {
        registry
    };
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    settings.set_ip_filter(Box::new(|ip| ip.is_loopback()));
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
                        assert_eq!(PlayoutDelayExtension::unmarshal(&mut bytes).unwrap(), PlayoutDelayExtension { min_delay: 0, max_delay: 3 });
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
                &[0, 0, 3]
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

// A small single-packet sample cannot expose a stall partway through a keyframe.
// Twelve frames also exercise sustained delivery beyond the default NACK cache.
// These synthetic NALs test transport/reassembly, not H264 decoder correctness.
async fn real_peer_fragmented_roundtrip(supported: bool, frame_count: usize, nal_bytes: usize) {
    const FRAME_DEADLINE: Duration = Duration::from_secs(2);
    const PRIMER: &[u8] = &[0, 0, 0, 1, 0x65, 0x88, 0x84];

    let publisher = api(true)
        .new_peer_connection(RTCConfiguration::default())
        .await
        .unwrap();
    let viewer = api(supported)
        .new_peer_connection(RTCConfiguration::default())
        .await
        .unwrap();
    let track = Arc::new(
        webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: "video/H264".into(),
                clock_rate: 90_000,
                ..Default::default()
            },
            "screen".into(),
            "fragmented-playout-test".into(),
        ),
    );
    let sender = publisher.add_track(track.clone()).await.unwrap();
    let rtcp_sender = sender.clone();
    let rtcp = tokio::spawn(async move { while rtcp_sender.read_rtcp().await.is_ok() {} });
    let (received, mut incoming) = tokio::sync::mpsc::channel(128);
    viewer.on_track(Box::new(move |track, _, _| {
        let received = received.clone();
        Box::pin(async move {
            // Drain continuously while write_sample sends the other fragments.
            // Stopping after the first packet would hide loss and partial frames.
            while let Ok((packet, _)) = track.read_rtp().await {
                if received.send((packet, Instant::now())).await.is_err() {
                    break;
                }
            }
        })
    }));

    let result = tokio::time::timeout(Duration::from_secs(40), async {
        let offer = publisher.create_offer(None).await.unwrap();
        assert!(offer.sdp.contains(URI));
        gather(&publisher, offer).await;
        viewer
            .set_remote_description(publisher.local_description().await.unwrap())
            .await
            .unwrap();
        let answer = viewer.create_answer(None).await.unwrap();
        assert_eq!(answer.sdp.contains(URI), supported);
        gather(&viewer, answer).await;
        publisher
            .set_remote_description(viewer.local_description().await.unwrap())
            .await
            .unwrap();
        let parameters = sender.get_parameters().await;
        let extension_id = parameters
            .rtp_parameters
            .header_extensions
            .iter()
            .find(|extension| extension.uri == URI)
            .map(|extension| extension.id as u8);
        assert_eq!(extension_id.is_some(), supported);

        let mut packetizer = crate::video_packets::VideoPackets::new();
        // A successful pre-DTLS write can discard media. Confirm actual receipt
        // with a tiny primer before timing large frames; never retry large frames.
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            loop {
                tokio::select! {
                    packet = incoming.recv() => {
                        let (packet, _) = packet.expect("primer RTP");
                        assert_eq!(packet.payload.as_ref(), &PRIMER[4..]);
                        assert!(packet.header.marker);
                        break;
                    }
                    _ = tick.tick() => {
                        for packet in packetizer.packetize(&bytes::Bytes::from_static(PRIMER), Instant::now()).unwrap() {
                            tokio::time::timeout(FRAME_DEADLINE, track.write_rtp_with_extensions(&packet, &[]))
                                .await.expect("primer write deadline").unwrap();
                        }
                    }
                }
            }
        }).await.expect("loopback media became ready");

        let mut last_sequence: Option<u16> = None;
        let mut last_timestamp: Option<u32> = None;
        let mut stream_ssrc = None;
        let capture_origin = Instant::now();
        for frame_index in 0..frame_count {
            let mut data = Vec::with_capacity(nal_bytes + 4);
            data.extend_from_slice(&[0, 0, 0, 1, 0x65]);
            // Nonzero bytes avoid accidental Annex B delimiters. Vary the bytes
            // across positions/frames so duplication or stale delivery fails.
            data.extend((0..nal_bytes - 1).map(|i| ((i * 37 + frame_index * 17) % 251 + 1) as u8));
            let sample = Sample {
                data: data.into(),
                duration: Duration::from_millis(20),
                ..Default::default()
            };
            let started = Instant::now();
            // Model lost capture frames halfway through the stream. The first
            // resumed RTP timestamp must include the gap, with no sequence hole.
            let skipped_ms = if frame_index >= 6 { 500 } else { 0 };
            let captured_at = capture_origin + Duration::from_millis(frame_index as u64 * 20 + skipped_ms);
            let packets = packetizer.packetize(&sample.data, captured_at).unwrap();
            let write = async {
                for packet in packets {
                    track.write_rtp_with_extensions(&packet, &[]).await.unwrap();
                }
                started.elapsed()
            };
            let receive = async {
                let mut reassembled = Vec::with_capacity(sample.data.len());
                let mut timestamp = None;
                let mut fragments = 0;
                let mut max_rtp_bytes = 0;
                loop {
                    let (packet, arrived) = incoming.recv().await.expect("fragmented RTP");
                    // At most a few primer packets can already be in flight.
                    if frame_index == 0 && fragments == 0 && packet.payload.as_ref() == &PRIMER[4..] {
                        continue;
                    }
                    let payload = &packet.payload;
                    assert!(payload.len() > 2, "empty FU-A payload");
                    assert_eq!(payload[0], 0x7c, "expected FU-A with IDR NRI");
                    assert_eq!(payload[1] & 0x3f, 5, "expected IDR NAL and no reserved bit");
                    let first = payload[1] & 0x80 != 0;
                    let last = payload[1] & 0x40 != 0;
                    assert_eq!(first, fragments == 0, "FU-A start must occur once");
                    assert_eq!(packet.header.marker, last, "marker must terminate the NAL");
                    if let Some(sequence) = last_sequence {
                        assert_eq!(packet.header.sequence_number, sequence.wrapping_add(1), "missing/reordered fragment");
                    }
                    last_sequence = Some(packet.header.sequence_number);
                    assert_eq!(*stream_ssrc.get_or_insert(packet.header.ssrc), packet.header.ssrc);
                    assert_eq!(*timestamp.get_or_insert(packet.header.timestamp), packet.header.timestamp);
                    if let Some(id) = extension_id {
                        let mut bytes = packet.header.get_extension(id).expect("playout hint on every fragment");
                        assert_eq!(PlayoutDelayExtension::unmarshal(&mut bytes).unwrap(), PlayoutDelayExtension { min_delay: 0, max_delay: 3 });
                    } else {
                        assert!(packet.header.extensions.is_empty(), "unnegotiated extension");
                    }
                    max_rtp_bytes = max_rtp_bytes.max(packet.marshal().unwrap().len());
                    if first {
                        reassembled.extend_from_slice(&[0, 0, 0, 1, (payload[0] & 0xe0) | (payload[1] & 0x1f)]);
                    }
                    reassembled.extend_from_slice(&payload[2..]);
                    fragments += 1;
                    assert!(reassembled.len() <= sample.data.len(), "frame overflow");
                    if packet.header.marker {
                        assert!(fragments >= 80, "large frame did not exercise enough fragments");
                        assert!(max_rtp_bytes <= 1212, "unexpected RTP size including extension");
                        // Do not print the frame contents on a failed assertion.
                        assert!(reassembled.as_slice() == sample.data.as_ref(), "reassembled frame differs: frame={frame_index} bytes={}", reassembled.len());
                        if let Some(previous) = last_timestamp {
                            assert_eq!(packet.header.timestamp, previous.wrapping_add(if frame_index == 6 { 46_800 } else { 1800 }), "capture gap must appear on the first resumed frame");
                        }
                        last_timestamp = timestamp;
                        break (fragments, max_rtp_bytes, arrived.duration_since(started));
                    }
                }
            };
            let (sent, received) = tokio::join!(
                tokio::time::timeout(FRAME_DEADLINE, write),
                tokio::time::timeout(FRAME_DEADLINE, receive),
            );
            let sent = sent.unwrap_or_else(|_| panic!("fragmented sample write stalled: playout={supported} frame={frame_index}"));
            let (fragments, max_rtp_bytes, delivered) = received.unwrap_or_else(|_| panic!("fragmented frame marker deadline: playout={supported} frame={frame_index}"));
            assert!(sent < FRAME_DEADLINE && delivered < FRAME_DEADLINE);
            eprintln!("fragmented RTP playout={supported} frame={frame_index} bytes={} fragments={fragments} max_rtp_bytes={max_rtp_bytes} write_ms={:.3} marker_ms={:.3}", sample.data.len(), sent.as_secs_f64() * 1000.0, delivered.as_secs_f64() * 1000.0);
        }
    }).await;
    publisher.close().await.unwrap();
    viewer.close().await.unwrap();
    rtcp.abort();
    result.expect("fragmented peer roundtrip deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_h264_frames_arrive_with_negotiated_playout_hint() {
    real_peer_fragmented_roundtrip(true, 12, 100 * 1024).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fragmented_h264_frames_arrive_without_playout_hint() {
    real_peer_fragmented_roundtrip(false, 12, 100 * 1024).await;
}

#[tokio::test]
async fn a_large_keyframe_is_not_rejected_by_an_artificial_packet_queue_limit() {
    real_peer_fragmented_roundtrip(true, 1, 1024 * 1024).await;
}
