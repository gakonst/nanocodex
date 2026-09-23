//! ICE configuration shared by Hand publishers and terminal viewers.
use serde_json::Value;
use std::{io, time::Duration};
use webrtc::api::setting_engine::SettingEngine;
use webrtc::ice_transport::ice_server::RTCIceServer;

pub fn ice_servers(
    value: &Value,
) -> std::result::Result<Vec<RTCIceServer>, Box<dyn std::error::Error + Send + Sync>> {
    value["iceServers"]
        .as_array()
        .filter(|s| s.len() <= 16)
        .ok_or_else(|| io::Error::other("invalid ICE servers"))?
        .iter()
        .map(|s| {
            let urls = match &s["urls"] {
                Value::String(s) => vec![s.clone()],
                Value::Array(urls) => urls
                    .iter()
                    .map(|u| u.as_str().map(str::to_owned).ok_or("invalid ICE URL"))
                    .collect::<std::result::Result<Vec<_>, _>>()?,
                _ => return Err("invalid ICE URLs".into()),
            };
            Ok(RTCIceServer {
                urls,
                username: s["username"].as_str().unwrap_or("").into(),
                credential: s["credential"].as_str().unwrap_or("").into(),
            })
        })
        .collect()
}

/// The publisher controls ICE nomination. The dependency's defaults wait 500ms
/// for srflx, 1s for prflx and 2s for relay, including the *remote* candidate's
/// type. A working Host→browserRelay pair otherwise sits idle for two seconds.
/// Give direct candidates a short head start without holding a validated path
/// for seconds. Normal ICE checks, priority, authentication and timeouts apply.
pub(crate) fn configure_screen_ice(settings: &mut SettingEngine) {
    settings.set_srflx_acceptance_min_wait(Some(Duration::from_millis(100)));
    settings.set_prflx_acceptance_min_wait(Some(Duration::from_millis(100)));
    settings.set_relay_acceptance_min_wait(Some(Duration::from_millis(250)));
}

#[cfg(test)]
#[path = "ice_tests.rs"]
mod tests;
