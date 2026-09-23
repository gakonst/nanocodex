use super::*;
use crate::input::Lease;
use webrtc::{
    api::{APIBuilder, setting_engine::SettingEngine},
    ice::network_type::NetworkType,
    peer_connection::{
        RTCPeerConnection, configuration::RTCConfiguration,
        sdp::session_description::RTCSessionDescription,
    },
};

async fn peer() -> Arc<RTCPeerConnection> {
    crate::tls::ensure_crypto_provider();
    let mut settings = SettingEngine::default();
    settings.set_include_loopback_candidate(true);
    settings.set_ip_filter(Box::new(|ip| ip.is_loopback()));
    settings.set_network_types(vec![NetworkType::Udp4]);
    // Accelerate loss detection only in this local protocol fixture.
    settings.set_ice_timeouts(
        Some(Duration::from_millis(300)),
        Some(Duration::from_secs(15)),
        Some(Duration::from_millis(50)),
    );
    Arc::new(
        APIBuilder::new()
            .with_setting_engine(settings)
            .build()
            .new_peer_connection(RTCConfiguration::default())
            .await
            .unwrap(),
    )
}
fn fixture(connection: &Arc<RTCPeerConnection>) -> (Arc<Recovery>, Task, mpsc::Receiver<Event>) {
    let microphone = Arc::new(Microphone::install(
        connection,
        Some(Arc::new(|| Box::pin(std::future::pending()))),
    ));
    let (events, incoming) = mpsc::channel(16);
    let (recovery, task) = Recovery::new(
        microphone,
        events,
        Arc::new(AtomicBool::new(false)),
        "fixture".into(),
    );
    (recovery, task, incoming)
}
async fn event(events: &mut mpsc::Receiver<Event>, kind: &str) {
    let event = tokio::time::timeout(Duration::from_secs(8), events.recv())
        .await
        .expect("missing peer lifecycle event")
        .unwrap();
    assert_eq!(event.value["type"], kind);
}

#[tokio::test(start_paused = true)]
async fn grace_is_bounded_per_outage_and_old_permissions_never_revive() {
    let connection = peer().await;
    let (recovery, task, mut events) = fixture(&connection);
    recovery.transition(State::Connected);
    let old = recovery.permission().unwrap();
    let mut lease = Lease::default();
    assert!(lease.acquire_connected("fixture", Some(old.clone())));
    let generation = lease.generation().to_owned();
    assert!(recovery.grant(&old, &generation));
    assert!(recovery.microphone(true, Duration::from_secs(10), &old));
    recovery.transition(State::Disconnected);
    assert!(!old.load(Ordering::Acquire));
    assert!(lease.expired());
    assert!(!lease.renew("fixture", &generation));
    assert!(!recovery.microphone.enabled());
    event(&mut events, "viewer_suspended").await;
    tokio::time::advance(Duration::from_secs(4)).await;
    recovery.transition(State::Connected);
    event(&mut events, "viewer_resumed").await;
    let fresh = recovery.permission().unwrap();
    assert!(!Arc::ptr_eq(&old, &fresh));
    assert!(!lease.valid("fixture", &generation));
    for kind in ["renew", "release"] {
        assert!(recovery.revoked_control(&json!({"type":kind,"generation":generation})));
        assert!(!recovery.revoked_control(&json!({"type":kind,"generation":"unknown"})));
        assert!(!recovery.revoked_control(&json!({"type":kind})));
    }
    assert!(!recovery.revoked_control(&json!({"type":"acquire","generation":generation})));
    assert!(!recovery.microphone(true, Duration::from_secs(10), &old));
    assert!(lease.acquire_connected("fixture", Some(fresh.clone())));
    assert_ne!(lease.generation(), generation);
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(
        events.try_recv().is_err(),
        "old timer retired recovered peer"
    );
    recovery.transition(State::Disconnected);
    event(&mut events, "viewer_suspended").await;
    tokio::time::advance(Duration::from_secs(4)).await;
    recovery.transition(State::Disconnected);
    tokio::time::advance(Duration::from_secs(1)).await;
    event(&mut events, "viewer_left").await;
    recovery.transition(State::Connected);
    assert!(recovery.permission().is_none(), "expired peer resurrected");
    drop(task);
    connection.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn terminal_states_and_owner_drop_do_not_wait_for_grace() {
    for terminal in [State::Closed, State::Failed] {
        let connection = peer().await;
        let (recovery, task, mut events) = fixture(&connection);
        recovery.transition(State::Connected);
        recovery.transition(State::Disconnected);
        event(&mut events, "viewer_suspended").await;
        recovery.transition(terminal);
        event(&mut events, "viewer_left").await;
        tokio::time::advance(GRACE).await;
        assert!(events.try_recv().is_err(), "duplicate terminal event");
        drop(task);
        connection.close().await.unwrap();
    }
    let connection = peer().await;
    let (recovery, task, mut events) = fixture(&connection);
    recovery.transition(State::Disconnected);
    event(&mut events, "viewer_left").await; // never established peers get no grace
    drop(task);
    let (recovery, task, mut events) = fixture(&connection);
    recovery.transition(State::Connected);
    let permission = recovery.permission().unwrap();
    recovery.transition(State::Disconnected);
    event(&mut events, "viewer_suspended").await;
    recovery.retire();
    drop(task); // same ownership as Peer::drop; no detached timer can affect a replacement
    tokio::time::advance(GRACE * 2).await;
    assert!(!permission.load(Ordering::Acquire));
    assert!(events.try_recv().is_err());
    connection.close().await.unwrap();
}

async fn gather(
    peer: &RTCPeerConnection,
    description: RTCSessionDescription,
) -> RTCSessionDescription {
    let mut complete = peer.gathering_complete_promise().await;
    peer.set_local_description(description).await.unwrap();
    complete.recv().await;
    peer.local_description().await.unwrap()
}
// A real loopback UDP forwarder drops encrypted WebRTC packets on request. No
// ICE state is injected; authenticated ICE consent loss and recovery drive the
// production state machine through the actual peer-connection callback.
fn through_proxy(
    mut description: RTCSessionDescription,
    port: u16,
) -> (RTCSessionDescription, std::net::SocketAddr) {
    let mut endpoint = None;
    description.sdp = description
        .sdp
        .lines()
        .map(|line| {
            if line.starts_with("a=candidate:") {
                let mut fields: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
                let address = format!("{}:{}", fields[4], fields[5]).parse().unwrap();
                assert!(endpoint.is_none_or(|old| old == address));
                endpoint = Some(address);
                fields[5] = port.to_string();
                fields.join(" ")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n";
    (
        description,
        endpoint.expect("fixture requires loopback candidate"),
    )
}

#[tokio::test]
async fn real_ice_outage_recovers_the_same_sctp_channel_without_restoring_authority() {
    let publisher = peer().await;
    let viewer = peer().await;
    let (recovery, timer, mut events) = fixture(&publisher);
    let state = recovery.clone();
    publisher.on_peer_connection_state_change(Box::new(move |next| {
        state.transition(next);
        Box::pin(async {})
    }));
    let (received, mut messages) = mpsc::channel(8);
    viewer.on_data_channel(Box::new(move |channel| {
        let received = received.clone();
        Box::pin(async move {
            channel.on_message(Box::new(move |message| {
                let received = received.clone();
                Box::pin(async move {
                    received.send(message.data).await.unwrap();
                })
            }));
        })
    }));
    let channel = publisher
        .create_data_channel("recovery-fixture", None)
        .await
        .unwrap();
    let (opened, mut ready) = mpsc::channel(1);
    channel.on_open(Box::new(move || {
        Box::pin(async move {
            opened.send(()).await.unwrap();
        })
    }));
    let proxy = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = proxy.local_addr().unwrap().port();
    let offer = gather(&publisher, publisher.create_offer(None).await.unwrap()).await;
    let (offer, publisher_address) = through_proxy(offer, port);
    viewer.set_remote_description(offer).await.unwrap();
    let answer = gather(&viewer, viewer.create_answer(None).await.unwrap()).await;
    let (answer, viewer_address) = through_proxy(answer, port);
    let dropping = Arc::new(AtomicBool::new(false));
    let drop_packets = dropping.clone();
    let proxy_task = Task(tokio::spawn(async move {
        let mut packet = [0; 65536];
        loop {
            let (size, from) = proxy.recv_from(&mut packet).await.unwrap();
            if drop_packets.load(Ordering::Acquire) {
                continue;
            }
            let to = if from == publisher_address {
                viewer_address
            } else if from == viewer_address {
                publisher_address
            } else {
                panic!("unknown fixture peer");
            };
            proxy.send_to(&packet[..size], to).await.unwrap();
        }
    }));
    publisher.set_remote_description(answer).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), ready.recv())
        .await
        .unwrap()
        .unwrap();
    channel.send_text("before").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), messages.recv())
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        b"before"
    );
    let permission = recovery.permission().unwrap();
    let mut lease = Lease::default();
    assert!(lease.acquire_connected("fixture", Some(permission.clone())));
    let generation = lease.generation().to_owned();
    assert!(recovery.grant(&permission, &generation));
    assert!(recovery.microphone(true, Duration::from_secs(10), &permission));
    dropping.store(true, Ordering::Release);
    event(&mut events, "viewer_suspended").await;
    assert_eq!(publisher.connection_state(), State::Disconnected);
    assert!(lease.expired());
    assert!(!lease.accept(
        "fixture",
        &json!({"generation":generation,"sequence":1,"kind":"key"})
    ));
    assert!(!recovery.microphone.enabled());
    dropping.store(false, Ordering::Release);
    event(&mut events, "viewer_resumed").await;
    assert_eq!(publisher.connection_state(), State::Connected);
    assert!(!lease.valid("fixture", &generation));
    assert!(!recovery.microphone.enabled());
    channel.send_text("after").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), messages.recv())
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        b"after"
    );
    assert!(lease.acquire_connected("fixture", recovery.permission()));
    assert_ne!(lease.generation(), generation);
    dropping.store(true, Ordering::Release);
    event(&mut events, "viewer_suspended").await;
    event(&mut events, "viewer_left").await; // actual sustained outage reaches the bounded grace
    assert!(recovery.permission().is_none());
    publisher.close().await.unwrap();
    viewer.close().await.unwrap();
    drop(timer);
    drop(proxy_task);
}

#[tokio::test(start_paused = true)]
async fn late_connected_callback_cannot_bypass_an_unpolled_expiry_timer() {
    let connection = peer().await;
    let (recovery, task, mut events) = fixture(&connection);
    drop(task); // Exercise the callback deadline without polling the timer.
    recovery.transition(State::Connected);
    recovery.transition(State::Disconnected);
    event(&mut events, "viewer_suspended").await;
    tokio::time::advance(GRACE).await;
    recovery.transition(State::Connected);
    event(&mut events, "viewer_left").await;
    assert!(recovery.permission().is_none());
    assert!(!recovery.connected.load(Ordering::Acquire));
    connection.close().await.unwrap();
}
