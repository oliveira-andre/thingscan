//! Checks whether saved devices still answer, using the endpoints recorded when they
//! were added. A LAN device that stopped answering is looked for again by its stable
//! ids, in case it came back under a new address.

use crate::device::{Device, Endpoint, url_host};
use crate::scan::{self, Channel};
use crate::{ble, cloud, mdns, ocf, ssdp};
use futures::FutureExt;
use futures::future::{BoxFuture, Shared};
use futures::stream::{FuturesUnordered, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

const PROBE_WAIT: Duration = Duration::from_millis(1500);
const BLE_LISTEN: Duration = Duration::from_secs(4);
const MDNS_LISTEN: Duration = Duration::from_secs(3);
const RELOCATE_SCAN: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Reachable,
    Unreachable,
    /// Nothing that could be checked (e.g. Bluetooth is off, or only a SoftAP is known).
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    /// "OCF", "UPnP", "TV API", "mDNS", "BLE" or "Wi-Fi".
    pub via: String,
    pub target: String,
    pub status: Status,
    pub detail: String,
    pub latency_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub id: String,
    pub status: Status,
    pub address: Option<String>,
    pub probes: Vec<Probe>,
    /// Set when the device answered at a new address; replaces its saved endpoints.
    pub relocated: Option<Vec<Endpoint>>,
}

#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub ids: Vec<String>,
    pub endpoints: Vec<Endpoint>,
}

type Pass<T> = Shared<BoxFuture<'static, Option<Result<T, String>>>>;

/// Check every target at once, reporting each as soon as its answer is known. `cloud` checks
/// SmartThings-account devices; without it they are reported as not checkable.
pub async fn check(targets: Vec<Target>, cloud: Option<cloud::Client>, mut on_report: impl FnMut(Report)) {
    let http = reqwest::Client::builder().timeout(PROBE_WAIT).build().unwrap_or_default();

    // One BLE listen and one mDNS browse are shared by every target that needs them.
    let needs_ble = targets.iter().flat_map(|t| &t.endpoints).any(|e| matches!(e, Endpoint::Ble { .. }));
    let ble: Pass<HashMap<String, i16>> = async move {
        if needs_ble { Some(ble::sightings(BLE_LISTEN).await.map_err(|e| format!("{e:#}"))) } else { None }
    }
    .boxed()
    .shared();
    let mut types: Vec<String> = targets
        .iter()
        .flat_map(|t| &t.endpoints)
        .filter_map(|e| match e {
            Endpoint::Mdns { service, .. } => Some(service.clone()),
            _ => None,
        })
        .collect();
    types.sort();
    types.dedup();
    let mdns: Pass<HashMap<String, String>> = async move {
        if types.is_empty() {
            None
        } else {
            Some(mdns::resolve(&types, MDNS_LISTEN).await.map_err(|e| format!("{e:#}")))
        }
    }
    .boxed()
    .shared();

    let mut checks: FuturesUnordered<_> =
        targets.iter().map(|t| check_one(t, &http, cloud.as_ref(), ble.clone(), mdns.clone())).collect();
    let mut lost = Vec::new();
    while let Some(report) = checks.next().await {
        let target = targets.iter().find(|t| t.id == report.id);
        if report.status == Status::Unreachable && target.is_some_and(|t| t.ids.iter().any(|id| is_lan_id(id))) {
            lost.push(report);
        } else {
            on_report(report);
        }
    }
    drop(checks);
    if lost.is_empty() {
        return;
    }

    let options = scan::Options {
        timeout: RELOCATE_SCAN,
        channels: vec![Channel::Ocf, Channel::Ssdp, Channel::Mdns],
        ..Default::default()
    };
    let devices = scan::run(options, |_| {}).await;
    for mut report in lost {
        let target = targets.iter().find(|t| t.id == report.id).expect("lost reports come from targets");
        if let Some(device) = devices.iter().find(|d| d.shares_identity(&target.ids)) {
            report.status = Status::Reachable;
            report.address = Some(device.address.clone());
            report.probes.push(Probe {
                via: device.sources.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("+"),
                target: device.address.clone(),
                status: Status::Reachable,
                detail: "found again at a new address".into(),
                latency_ms: None,
            });
            report.relocated = Some(relocated_endpoints(&target.endpoints, device));
        }
        on_report(report);
    }
}

async fn check_one(
    t: &Target,
    http: &reqwest::Client,
    cloud: Option<&cloud::Client>,
    ble: Pass<HashMap<String, i16>>,
    mdns: Pass<HashMap<String, String>>,
) -> Report {
    let probes =
        futures::future::join_all(t.endpoints.iter().map(|e| probe(e, http, cloud, ble.clone(), mdns.clone()))).await;
    Report {
        id: t.id.clone(),
        status: summarize(&probes),
        address: probes.iter().find(|p| p.status == Status::Reachable).map(|p| p.target.clone()),
        probes,
        relocated: None,
    }
}

async fn probe(
    endpoint: &Endpoint,
    http: &reqwest::Client,
    cloud: Option<&cloud::Client>,
    ble: Pass<HashMap<String, i16>>,
    mdns: Pass<HashMap<String, String>>,
) -> Probe {
    let started = Instant::now();
    let timed = |via: &str, target: String, result: Result<String, String>| {
        let latency = started.elapsed().as_millis() as u64;
        let (status, detail) = match result {
            Ok(detail) => (Status::Reachable, detail),
            Err(detail) => (Status::Unreachable, detail),
        };
        Probe { via: via.into(), target, status, detail, latency_ms: (status == Status::Reachable).then_some(latency) }
    };
    match endpoint {
        Endpoint::Ocf { ip, di } => timed("OCF", ip.to_string(), ocf::ping(*ip, di.as_deref(), PROBE_WAIT).await),
        Endpoint::Upnp { location, udn } => {
            timed("UPnP", url_host(location).to_string(), ssdp::check_upnp(http, location, udn.as_deref()).await)
        }
        Endpoint::SamsungTv { ip, id, .. } => {
            timed("TV API", ip.to_string(), ssdp::check_tv(http, *ip, id.as_deref()).await)
        }
        Endpoint::Mdns { fullname, .. } => {
            let (status, target, detail) = match mdns.await {
                Some(Ok(seen)) => match seen.get(fullname) {
                    Some(address) => (Status::Reachable, address.clone(), "advertising".to_string()),
                    None => (Status::Unreachable, fullname.clone(), "not advertising".to_string()),
                },
                Some(Err(e)) => (Status::Unknown, fullname.clone(), e),
                None => (Status::Unknown, fullname.clone(), "not checked".to_string()),
            };
            Probe { via: "mDNS".into(), target, status, detail, latency_ms: None }
        }
        Endpoint::Ble { id } => {
            let (status, detail) = match ble.await {
                Some(Ok(seen)) => match seen.get(id) {
                    Some(rssi) => (Status::Reachable, format!("{rssi} dBm")),
                    None => (Status::Unreachable, "not advertising".to_string()),
                },
                Some(Err(e)) => (Status::Unknown, e),
                None => (Status::Unknown, "not checked".to_string()),
            };
            Probe { via: "BLE".into(), target: id.clone(), status, detail, latency_ms: None }
        }
        Endpoint::SoftAp { ssid } => Probe {
            via: "Wi-Fi".into(),
            target: ssid.clone(),
            status: Status::Unknown,
            detail: "setup networks aren't checked".into(),
            latency_ms: None,
        },
        Endpoint::Cloud { device_id } => {
            let (status, detail) = match cloud {
                None => (Status::Unknown, "SmartThings isn't connected".to_string()),
                Some(client) => match client.health(device_id).await {
                    Ok(state) if state == "ONLINE" => (Status::Reachable, "online in SmartThings".to_string()),
                    Ok(state) => (Status::Unreachable, format!("{} in SmartThings", state.to_lowercase())),
                    Err(e) => (Status::Unknown, e.to_string()),
                },
            };
            let latency_ms = (status == Status::Reachable).then(|| started.elapsed().as_millis() as u64);
            Probe { via: "SmartThings".into(), target: device_id.clone(), status, detail, latency_ms }
        }
    }
}

/// Reachable if anything answered; unreachable if something checkable stayed silent.
pub fn summarize(probes: &[Probe]) -> Status {
    if probes.iter().any(|p| p.status == Status::Reachable) {
        Status::Reachable
    } else if probes.iter().any(|p| p.status == Status::Unreachable) {
        Status::Unreachable
    } else {
        Status::Unknown
    }
}

fn is_lan(e: &Endpoint) -> bool {
    matches!(e, Endpoint::Ocf { .. } | Endpoint::Upnp { .. } | Endpoint::SamsungTv { .. } | Endpoint::Mdns { .. })
}

fn is_lan_id(id: &str) -> bool {
    ["ocf:", "upnp:", "samsung-tv:", "mdns:"].iter().any(|p| id.starts_with(p))
}

/// Keep the saved Bluetooth / SoftAP endpoints and take the LAN ones from the new sighting.
pub fn relocated_endpoints(saved: &[Endpoint], found: &Device) -> Vec<Endpoint> {
    let mut endpoints: Vec<Endpoint> = saved.iter().filter(|e| !is_lan(e)).cloned().collect();
    endpoints.extend(found.endpoints.iter().filter(|e| is_lan(e)).cloned());
    endpoints
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Kind, Source};
    use std::collections::{BTreeMap, BTreeSet};

    fn probe(status: Status) -> Probe {
        Probe { via: "OCF".into(), target: "x".into(), status, detail: String::new(), latency_ms: None }
    }

    #[test]
    fn summarizes_probe_results() {
        assert_eq!(summarize(&[probe(Status::Unreachable), probe(Status::Reachable)]), Status::Reachable);
        assert_eq!(summarize(&[probe(Status::Unknown), probe(Status::Unreachable)]), Status::Unreachable);
        assert_eq!(summarize(&[probe(Status::Unknown)]), Status::Unknown);
        assert_eq!(summarize(&[]), Status::Unknown);
    }

    #[test]
    fn relocation_replaces_only_lan_endpoints() {
        let saved = vec![
            Endpoint::Ocf { ip: "192.168.3.7".parse().unwrap(), di: Some("d1".into()) },
            Endpoint::Ble { id: "p1".into() },
        ];
        let found = Device {
            key: "ocf:d1".into(),
            kind: Kind::Appliance,
            name: "A/C".into(),
            address: "192.168.3.9:49153".into(),
            sources: BTreeSet::from([Source::Ocf]),
            manufacturer: None,
            model: None,
            ids: vec!["ocf:d1".into()],
            endpoints: vec![Endpoint::Ocf { ip: "192.168.3.9".parse().unwrap(), di: Some("d1".into()) }],
            details: BTreeMap::new(),
        };
        let endpoints = relocated_endpoints(&saved, &found);
        assert_eq!(
            endpoints,
            vec![
                Endpoint::Ble { id: "p1".into() },
                Endpoint::Ocf { ip: "192.168.3.9".parse().unwrap(), di: Some("d1".into()) },
            ]
        );
    }
}
