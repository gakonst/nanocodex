use super::*;
use std::sync::Arc;
use tokio::sync::mpsc;
use webrtc::{
    api::{APIBuilder, setting_engine::SettingEngine},
    ice::network_type::NetworkType,
    ice_transport::ice_candidate_type::RTCIceCandidateType,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
};

async fn peer(publisher: bool) -> RTCPeerConnection {
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    settings.set_ip_filter(Box::new(|ip| ip.is_loopback()));
    settings.set_network_types(vec![NetworkType::Udp4]);
    if publisher {
        configure_screen_ice(&mut settings);
    }
    APIBuilder::new()
        .with_setting_engine(settings)
        .build()
        .new_peer_connection(RTCConfiguration::default())
        .await
        .unwrap()
}

async fn gather(peer: &RTCPeerConnection, description: RTCSessionDescription) {
    let mut complete = peer.gathering_complete_promise().await;
    peer.set_local_description(description).await.unwrap();
    complete.recv().await;
}

// Use real authenticated ICE, DTLS and SCTP over loopback; only the signaled
// remote candidate type changes. This isolates nomination latency from TURN
// provisioning and public network variability, not a simulation of a TURN hop.
async fn nomination_before_deadline(candidate_type: RTCIceCandidateType) {
    crate::tls::ensure_crypto_provider();
    let publisher = Arc::new(peer(true).await);
    let viewer = Arc::new(peer(false).await);
    let channel = publisher
        .create_data_channel("fixture", None)
        .await
        .unwrap();
    let (opened, mut opening) = mpsc::channel(1);
    channel.on_open(Box::new(move || {
        Box::pin(async move {
            let _ = opened.send(()).await;
        })
    }));
    let (selected, mut selection) = mpsc::channel(1);
    publisher
        .dtls_transport()
        .ice_transport()
        .on_selected_candidate_pair_change(Box::new(move |pair| {
            let selected = selected.clone();
            Box::pin(async move {
                let _ = selected.send((pair.local.typ, pair.remote.typ)).await;
            })
        }));
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        gather(&publisher, publisher.create_offer(None).await.unwrap()).await;
        viewer
            .set_remote_description(publisher.local_description().await.unwrap())
            .await
            .unwrap();
        gather(&viewer, viewer.create_answer(None).await.unwrap()).await;
        let mut answer = viewer.local_description().await.unwrap();
        assert!(
            answer.sdp.contains(" typ host"),
            "fixture needs a loopback candidate"
        );
        answer.sdp = answer
            .sdp
            .replace(" typ host", &format!(" typ {candidate_type}"));
        let started = std::time::Instant::now();
        publisher.set_remote_description(answer).await.unwrap();
        let connected = tokio::time::timeout(Duration::from_millis(800), opening.recv()).await;
        let elapsed = started.elapsed();
        let pair = selection.try_recv().ok();
        (connected, elapsed, pair)
    })
    .await;
    publisher.close().await.unwrap();
    viewer.close().await.unwrap();
    let (connected, elapsed, pair) = result.expect("loopback signaling timed out");
    assert!(
        matches!(connected, Ok(Some(()))),
        "validated {candidate_type} path still waiting for nomination after {elapsed:?}"
    );
    assert_eq!(pair, Some((RTCIceCandidateType::Host, candidate_type)));
    eprintln!("screen ICE Host→{candidate_type}: data channel opened in {elapsed:?}");
}

#[tokio::test]
async fn working_browser_relay_is_nominated_without_the_default_two_second_wait() {
    nomination_before_deadline(RTCIceCandidateType::Relay).await;
}

#[tokio::test]
async fn working_browser_prflx_is_nominated_without_the_default_one_second_wait() {
    nomination_before_deadline(RTCIceCandidateType::Prflx).await;
}

#[tokio::test]
async fn directly_reachable_browser_host_is_still_selected() {
    nomination_before_deadline(RTCIceCandidateType::Host).await;
}
