//! Wi-Fi SoftAP detection. Devices waiting to be added open their own access point;
//! the app holds `HotspotConfiguration` + `wifi-info` entitlements to find and join it.
//! SSID layout from st-device-sdk-c src/easysetup/discovery/ssid/iot_easysetup_discovery_ssid.c:
//!   E4: "<onboarding id>_E4<mnId:4><setupId:3><setupType:1><hashedSn:4><lastSn:4>"
//!   E5: "<onboarding id>_E5<mnId:4><setupId:3><hashedSn:4><lastSn:4>"

use crate::device::{Endpoint, Finding, Kind, Sink, Source};
use anyhow::{Result, anyhow, bail};
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub struct SetupSsid {
    pub onboarding_id: String,
    pub version: u8,
    pub mnid: String,
    pub setup_id: String,
    pub setup_type: Option<char>,
    pub hashed_serial: String,
    pub serial_tail: String,
}

pub fn parse_ssid(ssid: &str) -> Option<SetupSsid> {
    for (marker, version, tail_len) in [("_E4", 4u8, 16usize), ("_E5", 5, 15)] {
        let Some(pos) = ssid.rfind(marker) else { continue };
        let rest = &ssid[pos + marker.len()..];
        if rest.len() != tail_len || !rest.is_ascii() {
            continue;
        }
        let (mnid, rest) = rest.split_at(4);
        let (setup_id, rest) = rest.split_at(3);
        let (setup_type, rest) = if version == 4 {
            let c = rest.chars().next()?;
            if !c.is_ascii_digit() {
                continue;
            }
            (Some(c), &rest[1..])
        } else {
            (None, rest)
        };
        let (hashed_serial, serial_tail) = rest.split_at(4);
        return Some(SetupSsid {
            onboarding_id: ssid[..pos].to_string(),
            version,
            mnid: mnid.to_string(),
            setup_id: setup_id.to_string(),
            setup_type,
            hashed_serial: hashed_serial.to_string(),
            serial_tail: serial_tail.to_string(),
        });
    }
    None
}

#[derive(Debug)]
pub struct Network {
    pub ssid: String,
    pub bssid: Option<String>,
    pub signal: Option<String>,
}

pub async fn scan(timeout: Duration, sink: &Sink) -> Result<()> {
    let networks = tokio::time::timeout(timeout, list_networks())
        .await
        .map_err(|_| anyhow!("Wi-Fi scan didn't finish within {}s (it takes ~15s on macOS)", timeout.as_secs()))??;
    for n in networks {
        let Some(s) = parse_ssid(&n.ssid) else { continue };
        let name = if s.onboarding_id.is_empty() { n.ssid.clone() } else { s.onboarding_id.clone() };
        let mut f =
            Finding::new(Source::SoftAp, Kind::ReadyToAdd, name, n.bssid.clone().unwrap_or_else(|| n.ssid.clone()))
                .id(format!("ssid:{}", n.ssid))
                .endpoint(Endpoint::SoftAp { ssid: n.ssid.clone() })
                .detail("wifi.ssid", n.ssid.clone())
                .detail("st.mnid", s.mnid)
                .detail("st.setup_id", s.setup_id)
                .detail("st.ssid_version", format!("E{}", s.version))
                .detail("st.serial_tail", s.serial_tail)
                .detail("wifi.signal", n.signal.unwrap_or_default());
        if let Some(t) = s.setup_type {
            f = f.detail("st.setup_type", t.to_string());
        }
        let _ = sink.send(f);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn list_networks() -> Result<Vec<Network>> {
    let out = Command::new("system_profiler").args(["SPAirPortDataType", "-json"]).kill_on_drop(true).output().await?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let mut networks = Vec::new();
    let mut redacted = 0;
    for iface in json["SPAirPortDataType"][0]["spairport_airport_interfaces"].as_array().into_iter().flatten() {
        for net in iface["spairport_airport_other_local_wireless_networks"].as_array().into_iter().flatten() {
            let Some(ssid) = net["_name"].as_str() else { continue };
            if ssid == "<redacted>" {
                redacted += 1;
                continue;
            }
            networks.push(Network {
                ssid: ssid.to_string(),
                bssid: net["spairport_network_bssid"].as_str().map(str::to_string),
                signal: net["spairport_signal_noise"].as_str().map(str::to_string),
            });
        }
    }
    if networks.is_empty() && redacted > 0 {
        bail!(
            "macOS hid the names of {redacted} nearby networks; it shows them only to apps with Location Services access"
        );
    }
    Ok(networks)
}

#[cfg(target_os = "linux")]
async fn list_networks() -> Result<Vec<Network>> {
    let out = Command::new("nmcli")
        .args(["-t", "-f", "SSID,BSSID,SIGNAL", "dev", "wifi", "list", "--rescan", "yes"])
        .kill_on_drop(true)
        .output()
        .await?;
    if !out.status.success() {
        bail!("nmcli failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            // Terse mode escapes ':' inside fields as '\:'.
            let fields: Vec<String> =
                line.replace("\\:", "\u{0}").split(':').map(|f| f.replace('\u{0}', ":")).collect();
            let [ssid, bssid, signal] = fields.as_slice() else { return None };
            (!ssid.is_empty()).then(|| Network {
                ssid: ssid.clone(),
                bssid: Some(bssid.clone()),
                signal: Some(format!("{signal}%")),
            })
        })
        .collect())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
async fn list_networks() -> Result<Vec<Network>> {
    bail!("Wi-Fi scanning is not implemented on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_e4_ssid() {
        // "%s_E4%4s%3s%1d%4s%4s": onboarding id, mnId, setupId, setup type, hashed sn, last sn
        let s = parse_ssid("STDK_E4fTST0013Ab3f1234").unwrap();
        assert_eq!(s.onboarding_id, "STDK");
        assert_eq!(s.version, 4);
        assert_eq!(s.mnid, "fTST");
        assert_eq!(s.setup_id, "001");
        assert_eq!(s.setup_type, Some('3'));
        assert_eq!(s.hashed_serial, "Ab3f");
        assert_eq!(s.serial_tail, "1234");
    }

    #[test]
    fn parses_e5_ssid_with_underscores_in_prefix() {
        let s = parse_ssid("My_Lamp_E5fTST001Ab3f1234").unwrap();
        assert_eq!(s.onboarding_id, "My_Lamp");
        assert_eq!(s.version, 5);
        assert_eq!(s.setup_type, None);
    }

    #[test]
    fn ignores_ordinary_networks() {
        assert!(parse_ssid("HomeWiFi").is_none());
        assert!(parse_ssid("Router_E4short").is_none());
        assert!(parse_ssid("X_E4fTST001XAb3f1234").is_none());
    }
}
