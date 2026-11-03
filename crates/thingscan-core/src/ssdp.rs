//! SSDP / UPnP discovery. The app holds `com.apple.developer.networking.multicast`
//! and bundles Samsung's SmartView / TV controller frameworks, which find TVs this way.

use crate::device::{Endpoint, Finding, Kind, Sink, Source};
use crate::net;
use anyhow::Result;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::Instant;

const SEARCH_TARGETS: &[&str] = &[
    "ssdp:all",
    "upnp:rootdevice",
    "urn:samsung.com:device:RemoteControlReceiver:1",
    "urn:dial-multiscreen-org:service:dial:1",
];

#[derive(Debug, Clone)]
pub struct Response {
    pub from: SocketAddr,
    pub location: Option<String>,
    pub server: Option<String>,
    pub st: Option<String>,
    pub usn: Option<String>,
}

pub async fn scan(timeout: Duration, http: &reqwest::Client, enrich_tvs: bool, sink: &Sink) -> Result<()> {
    let group = net::v4_group(Ipv4Addr::new(239, 255, 255, 250), 1900);
    // Leave time after the last reply to fetch its description.
    let deadline = Instant::now() + timeout.saturating_sub(Duration::from_secs(2)).max(Duration::from_secs(2));

    let (replies_tx, mut replies) = mpsc::unbounded_channel::<Response>();
    for lan in net::ipv4_sockets()? {
        let replies_tx = replies_tx.clone();
        tokio::spawn(async move {
            for st in SEARCH_TARGETS {
                let msg = format!(
                    "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {st}\r\n\r\n"
                );
                let _ = lan.socket.send_to(msg.as_bytes(), group).await;
            }
            let mut buf = vec![0u8; 8192];
            while let Ok(Ok((n, from))) = tokio::time::timeout_at(deadline, lan.socket.recv_from(&mut buf)).await {
                if let Some(r) = parse_response(&String::from_utf8_lossy(&buf[..n]), from) {
                    let _ = replies_tx.send(r);
                }
            }
        });
    }
    drop(replies_tx);

    // Describe each LOCATION (or responder without one) as soon as it first appears.
    // A TV exposes several UPnP root devices; its REST API is asked once.
    let mut seen = HashSet::new();
    let enriched = Arc::new(Mutex::new(HashSet::new()));
    let mut lookups = JoinSet::new();
    while let Some(r) = replies.recv().await {
        if !seen.insert(r.location.clone().unwrap_or_else(|| r.from.ip().to_string())) {
            continue;
        }
        let (http, sink, enriched) = (http.clone(), sink.clone(), enriched.clone());
        lookups.spawn(async move {
            let mut f = describe(&http, r).await;
            if let (true, Kind::Tv, Some(ip)) = (enrich_tvs, f.kind, f.ip)
                && enriched.lock().unwrap().insert(ip)
            {
                f = enrich_samsung_tv(&http, ip, f).await;
            }
            let _ = sink.send(f);
        });
    }
    while lookups.join_next().await.is_some() {}
    Ok(())
}

pub fn parse_response(text: &str, from: SocketAddr) -> Option<Response> {
    let mut lines = text.split("\r\n");
    let status = lines.next()?;
    if !(status.starts_with("HTTP/1.1 200") || status.starts_with("NOTIFY")) {
        return None;
    }
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_uppercase(), v.trim().to_string());
        }
    }
    Some(Response {
        from,
        location: headers.remove("LOCATION"),
        server: headers.remove("SERVER"),
        st: headers.remove("ST").or_else(|| headers.remove("NT")),
        usn: headers.remove("USN"),
    })
}

#[derive(Debug, Default, PartialEq)]
pub struct Description {
    pub friendly_name: Option<String>,
    pub manufacturer: Option<String>,
    pub model_name: Option<String>,
    pub model_number: Option<String>,
    pub device_type: Option<String>,
    pub udn: Option<String>,
}

pub fn parse_description(xml: &str) -> Option<Description> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let device = doc.descendants().find(|n| n.tag_name().name() == "device")?;
    // Direct children only, so embedded sub-devices don't override the root device.
    let field = |name: &str| {
        device
            .children()
            .find(|c| c.tag_name().name() == name)
            .and_then(|c| c.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    Some(Description {
        friendly_name: field("friendlyName"),
        manufacturer: field("manufacturer"),
        model_name: field("modelName"),
        model_number: field("modelNumber"),
        device_type: field("deviceType"),
        udn: field("UDN"),
    })
}

async fn describe(http: &reqwest::Client, r: Response) -> Finding {
    let desc = match &r.location {
        Some(url) => match http.get(url).send().await {
            Ok(resp) => resp.text().await.ok().and_then(|x| parse_description(&x)),
            Err(_) => None,
        },
        None => None,
    }
    .unwrap_or_default();

    let samsung = [&desc.manufacturer, &r.server]
        .iter()
        .any(|s| s.as_deref().is_some_and(|s| s.to_ascii_lowercase().contains("samsung")));
    let tvish = [&desc.device_type, &r.st, &desc.model_name, &desc.friendly_name].iter().any(|s| {
        s.as_deref().is_some_and(|s| {
            s.contains("RemoteControlReceiver")
                || s.contains("dial-multiscreen")
                || s.contains("MediaRenderer")
                || s.contains("TV")
        })
    });
    let kind = match (samsung, tvish) {
        (true, true) => Kind::Tv,
        (true, false) => Kind::Samsung,
        _ => Kind::Other,
    };

    let ip = r.from.ip();
    let mut f = Finding::new(
        Source::Ssdp,
        kind,
        desc.friendly_name.clone().unwrap_or_else(|| ip.to_string()),
        r.from.to_string(),
    )
    .detail("upnp.device_type", desc.device_type.clone().unwrap_or_default())
    .detail("upnp.udn", desc.udn.clone().unwrap_or_default())
    .detail("upnp.usn", r.usn.clone().unwrap_or_default())
    .detail("upnp.location", r.location.clone().unwrap_or_default())
    .detail("upnp.server", r.server.clone().unwrap_or_default())
    .detail("upnp.model_number", desc.model_number.clone().unwrap_or_default());
    if let Some(udn) = &desc.udn {
        f = f.id(format!("upnp:{udn}"));
    }
    if let Some(location) = &r.location {
        f = f.endpoint(Endpoint::Upnp { location: location.clone(), udn: desc.udn.clone() });
    }
    f.ip = Some(ip);
    f.manufacturer = desc.manufacturer;
    f.model = desc.model_name;
    f
}

/// Samsung Tizen TVs expose device info at http://<ip>:8001/api/v2/ (public, unauthenticated).
async fn tv_info(http: &reqwest::Client, ip: IpAddr) -> Option<serde_json::Value> {
    http.get(format!("http://{ip}:8001/api/v2/")).send().await.ok()?.json().await.ok()
}

fn json_str(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

async fn enrich_samsung_tv(http: &reqwest::Client, ip: IpAddr, mut f: Finding) -> Finding {
    let Some(json) = tv_info(http, ip).await else { return f };
    let device = &json["device"];
    if let Some(name) = json_str(&device["name"]).or_else(|| json_str(&json["name"])) {
        f.name = name;
    }
    if let Some(model) = json_str(&device["modelName"]) {
        f.model = Some(model);
    }
    for (key, field) in [("tv.type", "type"), ("tv.os", "OS"), ("tv.wifi_mac", "wifiMac"), ("tv.power", "PowerState")] {
        if let Some(v) = json_str(&device[field]) {
            f.details.insert(key.into(), v);
        }
    }
    let id = json_str(&device["id"]).or_else(|| json_str(&json["id"]));
    if let Some(id) = &id {
        f = f.id(format!("samsung-tv:{id}"));
    }
    f.endpoint(Endpoint::SamsungTv { ip, id, mac: json_str(&device["wifiMac"]) })
}

/// A Samsung TV's Wi-Fi MAC address from its REST API, kept for Wake-on-LAN later.
pub async fn tv_wifi_mac(http: &reqwest::Client, ip: IpAddr) -> Option<String> {
    json_str(&tv_info(http, ip).await?["device"]["wifiMac"])
}

/// Re-fetch a saved description URL. Ok(detail) when it still describes the same device.
pub async fn check_upnp(http: &reqwest::Client, location: &str, udn: Option<&str>) -> Result<String, String> {
    let resp = http.get(location).send().await.map_err(|e| short_http_error(&e))?;
    let desc = parse_description(&resp.text().await.map_err(|e| short_http_error(&e))?)
        .ok_or_else(|| "not a UPnP description".to_string())?;
    match (udn, desc.udn.as_deref()) {
        (Some(want), Some(got)) if want != got => Err(format!("a different device ({got}) answers here now")),
        _ => Ok(desc.friendly_name.unwrap_or_else(|| "UPnP description".into())),
    }
}

/// Ask a Samsung TV's REST API. Ok(detail) includes its power state.
pub async fn check_tv(http: &reqwest::Client, ip: IpAddr, id: Option<&str>) -> Result<String, String> {
    let json = tv_info(http, ip).await.ok_or_else(|| "no answer on :8001".to_string())?;
    let got = json_str(&json["device"]["id"]).or_else(|| json_str(&json["id"]));
    match (id, got.as_deref()) {
        (Some(want), Some(got)) if want != got => Err("a different TV answers here now".into()),
        _ => Ok(match json_str(&json["device"]["PowerState"]) {
            Some(power) => format!("power {power}"),
            None => "TV API".into(),
        }),
    }
}

fn short_http_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection refused or host down".into()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_msearch_reply() {
        let text = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=1800\r\nLocation: http://192.168.1.20:9197/dmr\r\nSERVER: SHP, UPnP/1.0, Samsung UPnP SDK/1.0\r\nST: urn:samsung.com:device:RemoteControlReceiver:1\r\nUSN: uuid:abc::urn:samsung.com:device:RemoteControlReceiver:1\r\n\r\n";
        let r = parse_response(text, "192.168.1.20:1900".parse().unwrap()).unwrap();
        assert_eq!(r.location.as_deref(), Some("http://192.168.1.20:9197/dmr"));
        assert_eq!(r.st.as_deref(), Some("urn:samsung.com:device:RemoteControlReceiver:1"));
        assert!(parse_response("M-SEARCH * HTTP/1.1\r\n\r\n", r.from).is_none());
    }

    #[test]
    fn parses_root_device_not_embedded() {
        let xml = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device>
  <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
  <friendlyName>[TV] Living Room</friendlyName>
  <manufacturer>Samsung Electronics</manufacturer>
  <modelName>QN65Q80C</modelName>
  <UDN>uuid:1234</UDN>
  <deviceList><device><friendlyName>Embedded</friendlyName></device></deviceList>
</device></root>"#;
        let d = parse_description(xml).unwrap();
        assert_eq!(d.friendly_name.as_deref(), Some("[TV] Living Room"));
        assert_eq!(d.manufacturer.as_deref(), Some("Samsung Electronics"));
        assert_eq!(d.model_name.as_deref(), Some("QN65Q80C"));
        assert_eq!(d.udn.as_deref(), Some("uuid:1234"));
    }
}
