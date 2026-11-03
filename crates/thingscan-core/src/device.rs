use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::net::IpAddr;

/// Which discovery channel produced a finding. These mirror the channels the
/// SmartThings iOS app is set up for (Info.plist / entitlements / bundled SDKs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Mdns,
    Ssdp,
    Ocf,
    Ble,
    SoftAp,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Mdns => "mDNS",
            Source::Ssdp => "SSDP",
            Source::Ocf => "OCF",
            Source::Ble => "BLE",
            Source::SoftAp => "SoftAP",
        })
    }
}

/// Coarse category, ordered by how interesting it is to a "scan nearby" view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Advertising that it is waiting to be added (easy setup / Matter commissioning).
    ReadyToAdd,
    Hub,
    Tv,
    Appliance,
    Matter,
    ThreadBorderRouter,
    Tracker,
    Samsung,
    Other,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::ReadyToAdd => "Ready to add",
            Kind::Hub => "Hub",
            Kind::Tv => "TV",
            Kind::Appliance => "Appliance",
            Kind::Matter => "Matter",
            Kind::ThreadBorderRouter => "Thread BR",
            Kind::Tracker => "Tracker",
            Kind::Samsung => "Samsung",
            Kind::Other => "Other",
        })
    }
}

/// How to get back in touch with a device later, for reachability checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Endpoint {
    /// OCF over CoAP. Checked on the well-known port, since device ports are ephemeral.
    Ocf { ip: IpAddr, di: Option<String> },
    /// A UPnP device description URL.
    Upnp { location: String, udn: Option<String> },
    /// A Samsung TV's REST API on port 8001; `mac` is its Wi-Fi MAC, for Wake-on-LAN.
    SamsungTv {
        ip: IpAddr,
        id: Option<String>,
        #[serde(default)]
        mac: Option<String>,
    },
    /// A DNS-SD service instance, e.g. `Foo._matter._tcp.local.` of `_matter._tcp.local.`.
    Mdns { service: String, fullname: String },
    /// A BLE peripheral id (a CoreBluetooth UUID on macOS).
    Ble { id: String },
    /// A setup SoftAP network.
    SoftAp { ssid: String },
    /// A device in the user's SmartThings account, checked through the cloud API.
    Cloud { device_id: String },
}

/// Where scanners send findings as they happen.
pub type Sink = tokio::sync::mpsc::UnboundedSender<Finding>;

/// One observation from one channel.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub source: Source,
    pub kind: Kind,
    pub name: String,
    /// Human-readable locator: "ip:port", a BLE peripheral id, or a Wi-Fi BSSID.
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<IpAddr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Stable identities such as `ocf:<di>` or `upnp:<udn>`, used to recognise the
    /// device again on a later scan.
    pub ids: Vec<String>,
    pub endpoints: Vec<Endpoint>,
    pub details: BTreeMap<String, String>,
}

impl Finding {
    pub fn new(source: Source, kind: Kind, name: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            source,
            kind,
            name: name.into(),
            address: address.into(),
            ip: None,
            manufacturer: None,
            model: None,
            ids: Vec::new(),
            endpoints: Vec::new(),
            details: BTreeMap::new(),
        }
    }

    pub fn id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        if !self.ids.contains(&id) {
            self.ids.push(id);
        }
        self
    }

    pub fn endpoint(mut self, endpoint: Endpoint) -> Self {
        if !self.endpoints.contains(&endpoint) {
            self.endpoints.push(endpoint);
        }
        self
    }

    /// Identity of this observation within its channel, so a re-sighting replaces it.
    pub fn identity(&self) -> String {
        format!("{}|{}", self.source, self.ids.first().unwrap_or(&self.address))
    }

    pub fn detail(mut self, key: &str, value: impl Into<String>) -> Self {
        let value = value.into();
        if !value.is_empty() {
            self.details.insert(key.to_string(), value);
        }
        self
    }
}

/// A physical device, merged from every finding that shares an IP (LAN) or
/// locator (BLE / SoftAP).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// The first stable id, or the address when there is none.
    pub key: String,
    pub kind: Kind,
    pub name: String,
    pub address: String,
    pub sources: BTreeSet<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub ids: Vec<String>,
    pub endpoints: Vec<Endpoint>,
    pub details: BTreeMap<String, String>,
}

impl Device {
    pub fn shares_identity(&self, ids: &[String]) -> bool {
        self.ids.iter().any(|id| ids.contains(id))
    }
}

/// Name quality by source: protocol-level device names beat mDNS instance labels.
fn name_rank(source: Source) -> u8 {
    match source {
        Source::Ocf => 0,
        Source::Ssdp => 1,
        Source::Ble => 2,
        Source::SoftAp => 3,
        Source::Mdns => 4,
    }
}

pub fn merge(findings: Vec<Finding>) -> Vec<Device> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<Finding>> = HashMap::new();
    for f in findings {
        let key = match f.ip {
            Some(ip) => ip.to_string(),
            None => format!("{}:{}", f.source, f.address),
        };
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(f);
    }

    let mut devices: Vec<Device> = order
        .into_iter()
        .map(|key| {
            let mut group = groups.remove(&key).unwrap();
            group.sort_by_key(|f| (name_rank(f.source), f.kind));
            let best = &group[0];
            let mut ids: Vec<String> = Vec::new();
            let mut endpoints: Vec<Endpoint> = Vec::new();
            for f in &group {
                for id in &f.ids {
                    if !ids.contains(id) {
                        ids.push(id.clone());
                    }
                }
                for e in &f.endpoints {
                    // A TV exposes several UPnP root devices on one host; one is enough to check.
                    let same_host = |x: &Endpoint| match (x, e) {
                        (Endpoint::Upnp { location: a, .. }, Endpoint::Upnp { location: b, .. }) => {
                            strip_port(url_host(a)) == strip_port(url_host(b))
                        }
                        _ => false,
                    };
                    if !endpoints.contains(e) && !endpoints.iter().any(same_host) {
                        endpoints.push(e.clone());
                    }
                }
            }
            let mut device = Device {
                key: ids.first().cloned().unwrap_or_else(|| format!("addr:{}", best.address)),
                kind: group.iter().map(|f| f.kind).min().unwrap(),
                name: best.name.clone(),
                address: best.address.clone(),
                sources: group.iter().map(|f| f.source).collect(),
                manufacturer: group.iter().find_map(|f| f.manufacturer.clone()),
                model: group.iter().find_map(|f| f.model.clone()),
                ids,
                endpoints,
                details: BTreeMap::new(),
            };
            let multi = device.sources.len() > 1;
            if let (true, Some(ip)) = (multi, best.ip) {
                device.address = ip.to_string();
            }
            for f in &group {
                for (k, v) in &f.details {
                    let key = if multi { format!("{}.{}", f.source, k) } else { k.clone() };
                    match device.details.get_mut(&key) {
                        None => {
                            device.details.insert(key, v.clone());
                        }
                        Some(existing) if !existing.split(" | ").any(|e| e == v) => {
                            existing.push_str(" | ");
                            existing.push_str(v);
                        }
                        Some(_) => {}
                    }
                }
            }
            device
        })
        .collect();

    devices.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    devices
}

/// `host:port` of a URL.
pub fn url_host(url: &str) -> &str {
    url.split("://").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(url)
}

fn strip_port(host: &str) -> &str {
    match host.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_by_ip_and_prefers_protocol_names() {
        let ip: IpAddr = "192.168.1.20".parse().unwrap();
        let mut a = Finding::new(Source::Mdns, Kind::Matter, "A1B2-C3D4", "192.168.1.20:5540").id("mdns:a");
        a.ip = Some(ip);
        let mut b = Finding::new(Source::Ssdp, Kind::Tv, "Living Room TV", "192.168.1.20:9197")
            .id("upnp:uuid:1")
            .endpoint(Endpoint::Upnp { location: "http://192.168.1.20:9197/dmr".into(), udn: Some("uuid:1".into()) });
        b.ip = Some(ip);
        b.manufacturer = Some("Samsung Electronics".into());
        let ble = Finding::new(Source::Ble, Kind::Tracker, "SmartTag", "uuid-1");

        let devices = merge(vec![a, b, ble]);
        assert_eq!(devices.len(), 2);
        let tv = devices.iter().find(|d| d.kind == Kind::Tv).unwrap();
        assert_eq!(tv.name, "Living Room TV");
        assert_eq!(tv.sources.len(), 2);
        assert_eq!(tv.manufacturer.as_deref(), Some("Samsung Electronics"));
        assert_eq!(tv.key, "upnp:uuid:1");
        assert_eq!(tv.ids, vec!["upnp:uuid:1".to_string(), "mdns:a".to_string()]);
        assert_eq!(tv.endpoints.len(), 1);
        assert!(tv.shares_identity(&["mdns:a".to_string()]));
    }

    #[test]
    fn keeps_one_upnp_endpoint_per_host() {
        let ip: IpAddr = "192.168.3.16".parse().unwrap();
        let upnp = |path: &str, udn: &str| {
            let mut f = Finding::new(Source::Ssdp, Kind::Tv, "Tv", "192.168.3.16:9197")
                .id(format!("upnp:{udn}"))
                .endpoint(Endpoint::Upnp { location: format!("http://192.168.3.16:{path}"), udn: Some(udn.into()) });
            f.ip = Some(ip);
            f
        };
        let devices = merge(vec![upnp("9197/dmr", "a"), upnp("9119/screen_sharing", "b")]);
        assert_eq!(devices[0].endpoints.len(), 1);
        assert_eq!(devices[0].ids.len(), 2);
    }
}
