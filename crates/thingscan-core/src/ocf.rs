//! OCF (Open Connectivity Foundation) discovery over CoAP multicast. The app bundles
//! `OCFCore.framework` with an IoTivity client security DB (`oic_svr_db_client.dat`)
//! and the Samsung OCF root CA, which is how Samsung TVs and appliances are reached locally.

use crate::coap::{self, Message};
use crate::device::{Endpoint, Finding, Kind, Sink, Source};
use crate::net;
use anyhow::Result;
use ciborium::Value;
use std::collections::hash_map::RandomState;
use std::collections::{BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::task::JoinSet;
use tokio::time::Instant;

const PORT: u16 = 5683;
const ALL_OCF_V4: Ipv4Addr = Ipv4Addr::new(224, 0, 1, 187);
/// "All OCF Nodes" at link-, realm- and site-local scope.
const ALL_OCF_V6: [Ipv6Addr; 3] = [
    Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0x158),
    Ipv6Addr::new(0xff03, 0, 0, 0, 0, 0, 0, 0x158),
    Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 0, 0x158),
];

/// Random start (RFC 7252 §4.4): devices de-duplicate by sender address and message id,
/// so ids reused across runs or targets get silently dropped.
static MESSAGE_ID: LazyLock<AtomicU16> =
    LazyLock::new(|| AtomicU16::new(RandomState::new().build_hasher().finish() as u16));

fn next_id() -> u16 {
    MESSAGE_ID.fetch_add(1, Ordering::Relaxed)
}

/// Two flavours of discovery: legacy OIC 1.1 (no options) and OCF 1.x, which only
/// answers in its own content format when asked for it.
fn discovery_requests() -> Vec<Message> {
    let legacy = Message::get("/oic/res", None, next_id(), &[0x0c, 0xf1]);
    let ocf1 = Message::get("/oic/res", None, next_id(), &[0x0c, 0xf2])
        .with_option(coap::OPT_ACCEPT, coap::uint_bytes(coap::FORMAT_OCF_CBOR))
        .with_option(coap::OPT_OCF_ACCEPT_VERSION, coap::uint_bytes(coap::OCF_VERSION_1_0));
    vec![legacy, ocf1]
}

/// `sweep` also sends the probes unicast to every host on each local IPv4 subnet:
/// on the network this was built against, no Samsung device answered multicast `/oic/res`,
/// while all of them answered unicast.
pub async fn scan(timeout: Duration, sweep: bool, sink: &Sink) -> Result<()> {
    let discovery_deadline = Instant::now() + timeout.mul_f32(0.5);
    let wait = timeout.mul_f32(0.15).max(Duration::from_millis(500));
    let probes = discovery_requests();
    let mut tasks = Vec::new();

    for lan in net::ipv4_sockets()? {
        let probes = probes.clone();
        tasks.push(tokio::spawn(async move {
            let mut targets = vec![net::v4_group(ALL_OCF_V4, PORT)];
            if let (true, Some((ip, prefix))) = (sweep, lan.v4_net) {
                targets.extend(net::subnet_hosts(ip, prefix).into_iter().map(|h| net::v4_group(h, PORT)));
            }
            discover(&lan.socket, &targets, &probes, discovery_deadline, wait).await
        }));
    }
    for lan in net::ipv6_sockets()? {
        let probes = probes.clone();
        tasks.push(tokio::spawn(async move {
            let scope = lan.scope.unwrap_or(0);
            let targets: Vec<_> = ALL_OCF_V6.iter().map(|g| net::v6_group(*g, PORT, scope)).collect();
            discover(&lan.socket, &targets, &probes, discovery_deadline, wait).await
        }));
    }

    // Merge per device id; the same device answers over IPv4 and IPv6.
    let mut devices: HashMap<String, (SocketAddr, ResInfo)> = HashMap::new();
    for task in tasks {
        for (from, info) in task.await? {
            let key = info.di.clone().unwrap_or_else(|| from.ip().to_string());
            match devices.get_mut(&key) {
                Some((addr, existing)) => {
                    if addr.is_ipv6() && from.is_ipv4() {
                        *addr = from;
                    }
                    existing.absorb(info);
                }
                None => {
                    devices.insert(key, (from, info));
                }
            }
        }
    }

    let mut lookups = JoinSet::new();
    for (addr, info) in devices.into_values() {
        let sink = sink.clone();
        lookups.spawn(async move {
            let _ = sink.send(describe(addr, info, wait).await);
        });
    }
    while lookups.join_next().await.is_some() {}
    Ok(())
}

/// Unicast reachability check: `GET /oic/d` on the well-known port, since a device's own
/// ports change across reboots. Any reply proves it is up; a different `di` means another
/// device now holds that address. Ok(detail) on success.
pub async fn ping(ip: IpAddr, di: Option<&str>, wait: Duration) -> Result<String, String> {
    let to = SocketAddr::new(ip, PORT);
    let socket = bind_for(to).await.ok_or("couldn't open a UDP socket")?;
    let req = Message::get("/oic/d", None, next_id(), &[0x0d, 0x02]);
    let reply = request(&socket, to, &req, wait, |m| m.token == req.token).await.ok_or("no CoAP reply")?;
    if !reply.is_success() {
        // e.g. 4.01 when /oic/d needs DTLS; the device still answered.
        return Ok(format!("CoAP {}.{:02}", reply.code >> 5, reply.code & 0x1f));
    }
    let device: Option<Value> = ciborium::from_reader(reply.payload.as_slice()).ok();
    let got = device.as_ref().and_then(|d| text(get(d, "di")));
    match (di, got) {
        (Some(want), Some(got)) if want != got => Err(format!("a different OCF device ({got}) answers here now")),
        _ => Ok(device.as_ref().and_then(|d| text(get(d, "n"))).unwrap_or_else(|| "OCF".into())),
    }
}

/// Send every probe to every target, collect first replies until `deadline`, then finish
/// each block-wise reply on this same socket: IoTivity keys block transfers on the client
/// endpoint, and a follow-up from another port just gets an empty message back.
async fn discover(
    socket: &UdpSocket,
    targets: &[SocketAddr],
    probes: &[Message],
    deadline: Instant,
    wait: Duration,
) -> Vec<(SocketAddr, ResInfo)> {
    for target in targets {
        for probe in probes {
            let probe = Message { message_id: next_id(), ..probe.clone() };
            let _ = socket.send_to(&probe.encode(), *target).await;
        }
    }

    // First successful reply per responder; it answers once per probe and multicast scope.
    let mut firsts: Vec<(SocketAddr, Message)> = Vec::new();
    for (bytes, from) in net::collect(socket, deadline).await {
        let Some(msg) = Message::decode(&bytes).filter(Message::is_success) else { continue };
        if !firsts.iter().any(|(f, _)| *f == from) {
            firsts.push((from, msg));
        }
    }

    let mut out = Vec::new();
    for (from, first) in firsts {
        let Some(template) = probes.iter().find(|p| p.token == first.token) else { continue };
        if let Some(info) = complete(socket, from, template, first, wait).await.and_then(|p| parse_res(&p)) {
            out.push((from, info));
        }
    }
    out
}

const MAX_BLOCKS: usize = 128;

/// Follow CoAP block-wise transfer (RFC 7959) from an already-received first block.
async fn complete(
    socket: &UdpSocket,
    to: SocketAddr,
    template: &Message,
    first: Message,
    wait: Duration,
) -> Option<Vec<u8>> {
    let mut payload = first.payload.clone();
    let mut block = first.block2();
    for _ in 0..MAX_BLOCKS {
        let Some((_, true, szx)) = block else { return Some(payload) };
        // Derive the next block number from bytes received, in case the server changed block size.
        let num = (payload.len() >> (szx as usize + 4)) as u32;
        let req = template.for_block(next_id(), num, szx);
        let is_block = |m: &Message| m.token == template.token && m.block2().is_some_and(|b| b.0 == num);
        let reply = request(socket, to, &req, wait, is_block).await?;
        if !reply.is_success() {
            return None;
        }
        payload.extend_from_slice(&reply.payload);
        block = reply.block2();
    }
    None
}

/// Send `req` and return the first reply from `to` that `accept`s; stray or late
/// duplicates of earlier replies are skipped.
async fn request(
    socket: &UdpSocket,
    to: SocketAddr,
    req: &Message,
    wait: Duration,
    accept: impl Fn(&Message) -> bool,
) -> Option<Message> {
    socket.send_to(&req.encode(), to).await.ok()?;
    let deadline = Instant::now() + wait;
    let mut buf = vec![0u8; 8192];
    loop {
        let (n, from) = tokio::time::timeout_at(deadline, socket.recv_from(&mut buf)).await.ok()?.ok()?;
        if from.ip() != to.ip() {
            continue;
        }
        if let Some(msg) = Message::decode(&buf[..n]).filter(&accept) {
            return Some(msg);
        }
    }
}

async fn bind_for(to: SocketAddr) -> Option<UdpSocket> {
    let bind: SocketAddr = match to {
        SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
        SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    UdpSocket::bind(bind).await.ok()
}

#[derive(Debug, Default, PartialEq)]
pub struct ResInfo {
    pub di: Option<String>,
    pub resource_types: BTreeSet<String>,
    pub hrefs: BTreeSet<String>,
    pub endpoints: BTreeSet<String>,
}

impl ResInfo {
    fn absorb(&mut self, other: ResInfo) {
        if self.di.is_none() {
            self.di = other.di;
        }
        self.resource_types.extend(other.resource_types);
        self.hrefs.extend(other.hrefs);
        self.endpoints.extend(other.endpoints);
    }

    /// The `oic.d.*` device type(s), e.g. `oic.d.tv`, `oic.d.airconditioner`.
    pub fn device_types(&self) -> Vec<&str> {
        self.resource_types.iter().map(String::as_str).filter(|rt| rt.starts_with("oic.d.")).collect()
    }
}

fn get<'a>(map: &'a Value, key: &str) -> Option<&'a Value> {
    map.as_map()?.iter().find(|(k, _)| k.as_text() == Some(key)).map(|(_, v)| v)
}

fn text(v: Option<&Value>) -> Option<String> {
    v?.as_text().map(str::to_string)
}

fn texts(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(|i| i.as_text().map(str::to_string)).collect(),
        Some(Value::Text(t)) => vec![t.clone()],
        _ => vec![],
    }
}

/// Parse an `/oic/res` payload. Handles the OIC 1.1 shape (`[{di, links: [...]}]`),
/// the OCF 1.x link-list shape (`[{anchor: "ocf://<di>", href, rt, eps}]`), and a
/// baseline map with a `links` array.
pub fn parse_res(payload: &[u8]) -> Option<ResInfo> {
    let value: Value = ciborium::from_reader(payload).ok()?;
    let mut info = ResInfo::default();

    let mut links: Vec<&Value> = Vec::new();
    let top: Vec<&Value> = match &value {
        Value::Array(items) => items.iter().collect(),
        Value::Map(_) => vec![&value],
        _ => return None,
    };
    for item in top {
        if let Some(di) = text(get(item, "di")) {
            info.di.get_or_insert(di);
        }
        match get(item, "links") {
            Some(Value::Array(inner)) => links.extend(inner.iter()),
            _ => links.push(item),
        }
    }

    for link in links {
        if let Some(anchor) = text(get(link, "anchor"))
            && let Some(di) = anchor.strip_prefix("ocf://")
        {
            info.di.get_or_insert(di.trim_end_matches('/').to_string());
        }
        if let Some(href) = text(get(link, "href")) {
            info.hrefs.insert(href);
        }
        info.resource_types.extend(texts(get(link, "rt")));
        if let Some(Value::Array(eps)) = get(link, "eps") {
            info.endpoints.extend(eps.iter().filter_map(|ep| text(get(ep, "ep"))));
        }
    }

    (info.di.is_some() || !info.hrefs.is_empty()).then_some(info)
}

async fn get_resource(to: SocketAddr, path: &str, wait: Duration) -> Option<Value> {
    let socket = bind_for(to).await?;
    let req = Message::get(path, None, next_id(), &[0x0d, 0x01]);
    let first = request(&socket, to, &req, wait, |m| m.token == req.token).await?;
    if !first.is_success() {
        return None;
    }
    let payload = complete(&socket, to, &req, first, wait).await?;
    ciborium::from_reader(payload.as_slice()).ok()
}

async fn describe(addr: SocketAddr, info: ResInfo, wait: Duration) -> Finding {
    // /oic/d and /oic/p are often readable without DTLS; when they aren't, /oic/res is all we get.
    let (device, platform) = tokio::join!(get_resource(addr, "/oic/d", wait), get_resource(addr, "/oic/p", wait));
    let name = device.as_ref().and_then(|d| text(get(d, "n")));
    let device_types = info.device_types();

    let kind = if device_types.contains(&"oic.d.tv") {
        Kind::Tv
    } else if device_types.contains(&"oic.d.bridge") {
        Kind::Hub
    } else if !device_types.is_empty() {
        Kind::Appliance
    } else {
        Kind::Other
    };
    let fallback = device_types
        .first()
        .map(|t| format!("OCF {}", t.trim_start_matches("oic.d.")))
        .unwrap_or_else(|| "OCF device".to_string());

    let mut f = Finding::new(Source::Ocf, kind, name.unwrap_or(fallback), addr.to_string())
        .detail("ocf.di", info.di.clone().unwrap_or_default())
        .detail("ocf.device_types", device_types.join(","))
        .detail("ocf.resources", info.hrefs.iter().cloned().collect::<Vec<_>>().join(","))
        .detail("ocf.endpoints", info.endpoints.iter().cloned().collect::<Vec<_>>().join(","));
    if let Some(d) = &device {
        f = f
            .detail("ocf.spec_version", text(get(d, "icv")).unwrap_or_default())
            .detail("ocf.data_model", texts(get(d, "dmv")).join(","));
    }
    if let Some(p) = &platform {
        f.manufacturer = text(get(p, "mnmn"));
        let model_number = text(get(p, "mnmo"));
        f.model = model_number.as_deref().and_then(|m| m.split('|').next()).map(str::to_string);
        if model_number.as_deref().is_some_and(|m| m.contains('|')) {
            f = f.detail("ocf.model_number", model_number.unwrap_or_default());
        }
        f = f.detail("ocf.platform_id", text(get(p, "pi")).unwrap_or_default());
    }
    if f.manufacturer.is_none() && info.resource_types.iter().any(|rt| rt.starts_with("x.com.samsung")) {
        f = f.detail("ocf.vendor_hint", "Samsung (x.com.samsung.* resources)");
    }
    if let Some(di) = &info.di {
        f = f.id(format!("ocf:{di}"));
    }
    // A link-local IPv6 address is useless without its interface scope, so only keep others.
    f.ip = Some(addr.ip()).filter(|ip| !matches!(ip, IpAddr::V6(v6) if v6.is_unicast_link_local()));
    if let Some(ip) = f.ip {
        f = f.endpoint(Endpoint::Ocf { ip, di: info.di.clone() });
    }
    f
}

#[cfg(test)]
mod tests {
    use super::*;
    use ciborium::cbor;

    fn encode(v: Value) -> Vec<u8> {
        let mut out = Vec::new();
        ciborium::into_writer(&v, &mut out).unwrap();
        out
    }

    #[test]
    fn parses_legacy_oic_res() {
        let v = cbor!([{
            "di" => "f1b2c3d4-0000-0000-0000-000000000001",
            "links" => [
                { "href" => "/oic/d", "rt" => ["oic.wk.d", "oic.d.tv"] },
                { "href" => "/sec/tvinfo", "rt" => ["x.com.samsung.tv.deviceinfo"] }
            ]
        }])
        .unwrap();
        let info = parse_res(&encode(v)).unwrap();
        assert_eq!(info.di.as_deref(), Some("f1b2c3d4-0000-0000-0000-000000000001"));
        assert_eq!(info.device_types(), vec!["oic.d.tv"]);
        assert!(info.hrefs.contains("/sec/tvinfo"));
    }

    #[test]
    fn parses_ocf1_link_list() {
        let v = cbor!([
            {
                "anchor" => "ocf://8a1b-22",
                "href" => "/oic/d",
                "rt" => ["oic.wk.d", "oic.d.airconditioner"],
                "eps" => [{ "ep" => "coaps://[fe80::1]:49152" }]
            },
            { "anchor" => "ocf://8a1b-22", "href" => "/temperature", "rt" => "oic.r.temperature" }
        ])
        .unwrap();
        let info = parse_res(&encode(v)).unwrap();
        assert_eq!(info.di.as_deref(), Some("8a1b-22"));
        assert_eq!(info.device_types(), vec!["oic.d.airconditioner"]);
        assert!(info.resource_types.contains("oic.r.temperature"));
        assert!(info.endpoints.contains("coaps://[fe80::1]:49152"));
    }

    #[test]
    fn rejects_non_discovery_payloads() {
        assert!(parse_res(&encode(cbor!("hello").unwrap())).is_none());
        assert!(parse_res(&[]).is_none());
    }
}
