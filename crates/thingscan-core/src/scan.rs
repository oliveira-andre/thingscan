//! Runs the discovery channels together and merges what they find as it arrives.

use crate::device::{self, Device, Finding, Source};
use crate::{ble, mdns, ocf, softap, ssdp};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Ocf,
    Ssdp,
    Mdns,
    Ble,
    Wifi,
}

impl Channel {
    pub const ALL: [Channel; 5] = [Channel::Ocf, Channel::Ssdp, Channel::Mdns, Channel::Ble, Channel::Wifi];

    pub fn source(self) -> Source {
        match self {
            Channel::Ocf => Source::Ocf,
            Channel::Ssdp => Source::Ssdp,
            Channel::Mdns => Source::Mdns,
            Channel::Ble => Source::Ble,
            Channel::Wifi => Source::SoftAp,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Ocf => "OCF",
            Channel::Ssdp => "SSDP",
            Channel::Mdns => "mDNS",
            Channel::Ble => "BLE",
            Channel::Wifi => "Wi-Fi",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub timeout: Duration,
    /// The macOS Wi-Fi scan alone takes ~15s, so it has its own budget.
    pub wifi_timeout: Duration,
    pub channels: Vec<Channel>,
    pub all_ble: bool,
    pub mdns_types: Vec<String>,
    pub ocf_sweep: bool,
    pub enrich_tvs: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(8),
            wifi_timeout: Duration::from_secs(8),
            channels: Channel::ALL.to_vec(),
            all_ble: false,
            mdns_types: Vec::new(),
            ocf_sweep: true,
            enrich_tvs: true,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Event {
    Started {
        channel: Channel,
    },
    /// Every device known so far, merged; sent after each new or updated finding.
    Devices {
        devices: Vec<Device>,
    },
    Done {
        channel: Channel,
        found: usize,
        error: Option<String>,
    },
    /// Every channel has finished.
    Finished {
        devices: Vec<Device>,
    },
}

/// Scan every selected channel at once. Dropping the returned future stops the scan.
pub async fn run(options: Options, mut on_event: impl FnMut(Event)) -> Vec<Device> {
    let (sink, mut found) = mpsc::unbounded_channel::<Finding>();
    let (done_tx, mut done) = mpsc::unbounded_channel::<(Channel, Option<String>)>();
    let http = reqwest::Client::builder().timeout(Duration::from_secs(2)).build().unwrap_or_default();

    let mut channels = options.channels.clone();
    channels.dedup();
    let mut tasks = JoinSet::new();
    for &channel in &channels {
        on_event(Event::Started { channel });
        let (o, sink, done_tx, http) = (options.clone(), sink.clone(), done_tx.clone(), http.clone());
        tasks.spawn(async move {
            let result = match channel {
                Channel::Ocf => ocf::scan(o.timeout, o.ocf_sweep, &sink).await,
                Channel::Ssdp => ssdp::scan(o.timeout, &http, o.enrich_tvs, &sink).await,
                Channel::Mdns => mdns::scan(o.timeout, &o.mdns_types, &sink).await,
                Channel::Ble => ble::scan(o.timeout, o.all_ble, &sink).await,
                Channel::Wifi => softap::scan(o.wifi_timeout, &sink).await,
            };
            let _ = done_tx.send((channel, result.err().map(|e| format!("{e:#}"))));
        });
    }
    drop((sink, done_tx));

    let mut findings: Vec<Finding> = Vec::new();
    let mut pending = channels.len();
    while pending > 0 {
        tokio::select! {
            Some(f) = found.recv() => {
                upsert(&mut findings, f);
                on_event(Event::Devices { devices: device::merge(findings.clone()) });
            }
            Some((channel, error)) = done.recv() => {
                pending -= 1;
                // Anything the channel sent before finishing is already queued; count it.
                let mut drained = false;
                while let Ok(f) = found.try_recv() {
                    upsert(&mut findings, f);
                    drained = true;
                }
                if drained {
                    on_event(Event::Devices { devices: device::merge(findings.clone()) });
                }
                let count = findings.iter().filter(|f| f.source == channel.source()).count();
                on_event(Event::Done { channel, found: count, error });
            }
            else => break,
        }
    }
    let devices = device::merge(findings);
    on_event(Event::Finished { devices: devices.clone() });
    devices
}

/// A re-sighting (another mDNS resolution, a BLE advert update) replaces the earlier one.
fn upsert(findings: &mut Vec<Finding>, f: Finding) {
    let identity = f.identity();
    match findings.iter_mut().find(|e| e.identity() == identity) {
        Some(existing) => *existing = f,
        None => findings.push(f),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Kind;

    #[test]
    fn resightings_replace_earlier_findings() {
        let mut findings = Vec::new();
        upsert(&mut findings, Finding::new(Source::Ble, Kind::Samsung, "Samsung BLE device", "p1").id("ble:p1"));
        upsert(&mut findings, Finding::new(Source::Ble, Kind::ReadyToAdd, "Room A/C", "p1").id("ble:p1"));
        upsert(&mut findings, Finding::new(Source::Ble, Kind::Tracker, "Tag", "p2").id("ble:p2"));
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].name, "Room A/C");
    }
}
