//! ICE configuration shared by Hand publishers and terminal viewers.
use serde_json::Value;
use std::io;
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
