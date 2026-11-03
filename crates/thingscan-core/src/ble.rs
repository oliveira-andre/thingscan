//! BLE advertisement scan. The app asks for Bluetooth "to scan for nearby devices
//! using Bluetooth Low Energy (BLE)" (Base.lproj/Localizable.strings) and bundles a
//! D2D (device-to-device) stack; the advert formats below come from Samsung's public
//! SmartThings Device SDK and the Matter spec.

use crate::device::{Endpoint, Finding, Kind, Sink, Source, hex};
use crate::matter;
use anyhow::{Context, Result, anyhow};
use btleplug::api::{Central, Manager as _, Peripheral as _, PeripheralProperties, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;
use uuid::Uuid;

pub const SAMSUNG_COMPANY_ID: u16 = 0x0075;
/// Matter (CHIPoBLE) commissionable advertisement service UUID.
pub const MATTER_SERVICE: Uuid = uuid16(0xFFF6);
/// 16-bit member UUIDs assigned to Samsung Electronics; 0xFD5A is used by Galaxy SmartTag.
pub const SAMSUNG_SERVICES: [(Uuid, &str); 3] = [
    (uuid16(0xFD5A), "Galaxy SmartTag / Samsung Find"),
    (uuid16(0xFD59), "Samsung service 0xFD59"),
    (uuid16(0xFD69), "Samsung service 0xFD69"),
];

const fn uuid16(short: u16) -> Uuid {
    Uuid::from_u128(((short as u128) << 96) | 0x0000_0000_0000_1000_8000_0080_5F9B_34FB)
}

const PERMISSION_HINT: &str =
    "on macOS allow Bluetooth for your terminal app (System Settings > Privacy & Security > Bluetooth)";

pub async fn scan(timeout: Duration, include_all: bool, sink: &Sink) -> Result<()> {
    bounded(timeout, async {
        let adapter = start().await?;
        let deadline = Instant::now() + timeout;
        // Re-send a peripheral only when what we know about it changes (RSSI aside).
        let mut sent: HashMap<String, String> = HashMap::new();
        loop {
            for peripheral in adapter.peripherals().await? {
                let Ok(Some(props)) = peripheral.properties().await else { continue };
                let id = peripheral.id().to_string();
                let Some(f) = classify(&id, &props, include_all) else { continue };
                let signature = format!(
                    "{:?}|{}|{:?}",
                    f.kind,
                    f.name,
                    f.details.iter().filter(|(k, _)| *k != "rssi").collect::<Vec<_>>()
                );
                if sent.get(&id) != Some(&signature) {
                    sent.insert(id, signature);
                    let _ = sink.send(f);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            tokio::time::sleep((deadline - now).min(Duration::from_secs(1))).await;
        }
        let _ = adapter.stop_scan().await;
        Ok(())
    })
    .await
}

/// Listen for `duration` and return the RSSI of every peripheral heard, by id.
pub async fn sightings(duration: Duration) -> Result<HashMap<String, i16>> {
    bounded(duration, async {
        let adapter = start().await?;
        tokio::time::sleep(duration).await;
        let _ = adapter.stop_scan().await;
        let mut out = HashMap::new();
        for peripheral in adapter.peripherals().await? {
            if let Ok(Some(PeripheralProperties { rssi: Some(rssi), .. })) = peripheral.properties().await {
                out.insert(peripheral.id().to_string(), rssi);
            }
        }
        Ok(out)
    })
    .await
}

/// CoreBluetooth can block while a permission prompt is pending, so bound every use of it.
async fn bounded<T>(duration: Duration, work: impl Future<Output = Result<T>>) -> Result<T> {
    tokio::time::timeout(duration + Duration::from_secs(3), work)
        .await
        .map_err(|_| anyhow!("Bluetooth didn't respond; {PERMISSION_HINT}"))?
}

async fn start() -> Result<Adapter> {
    let manager = Manager::new().await.context("Bluetooth unavailable")?;
    let adapter = manager.adapters().await?.into_iter().next().context("no Bluetooth adapter found")?;
    adapter.start_scan(ScanFilter::default()).await.with_context(|| format!("BLE scan failed; {PERMISSION_HINT}"))?;
    Ok(adapter)
}

pub fn classify(id: &str, props: &PeripheralProperties, include_all: bool) -> Option<Finding> {
    let local_name = props.local_name.clone().or_else(|| props.advertisement_name.clone());
    let mut f = if let Some(onb) = props.manufacturer_data.get(&SAMSUNG_COMPANY_ID).and_then(|d| parse_onboarding(d)) {
        let name = local_name.clone().unwrap_or_else(|| format!("SmartThings device {}", onb.mnid));
        let kind = if onb.owned { Kind::Samsung } else { Kind::ReadyToAdd };
        let mut f = Finding::new(Source::Ble, kind, name, id)
            .detail("st.ocf_info", onb.ocf_flags_description())
            .detail("st.mnid", onb.mnid)
            .detail("st.setup_id", onb.setup_id)
            .detail("st.packet_version", format!("0x{:02x}", onb.packet_version));
        if let Some(mac) = onb.bt_mac {
            f = f.id(format!("btmac:{mac}")).detail("st.bt_mac", mac);
        }
        if !onb.extra.is_empty() {
            f = f.detail("st.extra", hex(&onb.extra));
        }
        for (kind, value) in &onb.tlv {
            let key = if *kind == 0x03 { "st.hybrid_serial".to_string() } else { format!("st.tlv.0x{kind:02x}") };
            f = f.detail(&key, ascii(value).unwrap_or_else(|| hex(value)));
        }
        f
    } else if let Some(adv) = props.service_data.get(&MATTER_SERVICE).and_then(|d| matter::parse_ble_service_data(d)) {
        let mut f = Finding::new(
            Source::Ble,
            Kind::ReadyToAdd,
            local_name.clone().unwrap_or_else(|| format!("Matter device {:04}", adv.discriminator)),
            id,
        )
        .detail("matter.discriminator", adv.discriminator.to_string())
        .detail("matter.vendor_id", adv.vendor_id.to_string())
        .detail("matter.product_id", adv.product_id.to_string());
        if adv.opcode != 0 {
            f = f.detail("matter.opcode", format!("0x{:02x}", adv.opcode));
        }
        f
    } else if let Some((_, label)) =
        SAMSUNG_SERVICES.iter().find(|(uuid, _)| props.services.contains(uuid) || props.service_data.contains_key(uuid))
    {
        let kind = if label.contains("SmartTag") { Kind::Tracker } else { Kind::Samsung };
        let mut f = Finding::new(Source::Ble, kind, local_name.clone().unwrap_or_else(|| label.to_string()), id);
        f.manufacturer = Some("Samsung".into());
        f
    } else if props.manufacturer_data.contains_key(&SAMSUNG_COMPANY_ID) {
        let mut f = Finding::new(
            Source::Ble,
            Kind::Samsung,
            local_name.clone().unwrap_or_else(|| "Samsung BLE device".into()),
            id,
        );
        f.manufacturer = Some("Samsung".into());
        f
    } else if include_all {
        Finding::new(Source::Ble, Kind::Other, local_name.clone().unwrap_or_else(|| "(unnamed)".into()), id)
    } else {
        return None;
    };

    // The peripheral id goes first: it is what a later sighting is matched on.
    f.ids.insert(0, format!("ble:{id}"));
    f = f.endpoint(Endpoint::Ble { id: id.to_string() });
    if let Some(rssi) = props.rssi {
        f = f.detail("rssi", format!("{rssi} dBm"));
    }
    for (company, data) in &props.manufacturer_data {
        f = f.detail(&format!("mfr.0x{company:04x}"), hex(data));
    }
    for (uuid, data) in &props.service_data {
        f = f.detail(&format!("svc.{uuid}"), hex(data));
    }
    Some(f)
}

/// SmartThings easy-setup advert, manufacturer data for company 0x0075 (company id stripped):
/// control(1) service_id(1)=0x0C packet_version(1) ocf_info(1) feature(1) mnId(4) setupId(3)
/// setup_network(1) address_transfer(1) bt_mac(6) [extra] tlv_len(1) {type(1) len(1) value}*
/// See st-device-sdk-c src/easysetup/discovery/advertiser/iot_easysetup_discovery_advertiser.c.
#[derive(Debug, PartialEq, Eq)]
pub struct Onboarding {
    pub packet_version: u8,
    pub ocf_flags: u8,
    pub owned: bool,
    pub mnid: String,
    pub setup_id: String,
    pub bt_mac: Option<String>,
    /// Bytes between the MAC and the TLV block. STDK has none; Samsung appliances send one.
    pub extra: Vec<u8>,
    /// Custom-data TLV records as (type, value). Type 0x03 is STDK's hybrid serial number.
    pub tlv: Vec<(u8, Vec<u8>)>,
}

const SERVICE_ID_SAMSUNG_CONNECT: u8 = 0x0C;

impl Onboarding {
    pub fn ocf_flags_description(&self) -> String {
        let mut parts = Vec::new();
        if self.ocf_flags & 0x01 != 0 {
            parts.push("onboarding supported");
        }
        if self.ocf_flags & 0x02 != 0 {
            parts.push("owned");
        }
        if self.ocf_flags & 0x04 != 0 {
            parts.push("onboarding ready");
        }
        format!("0x{:02x} ({})", self.ocf_flags, parts.join(", "))
    }
}

pub fn parse_onboarding(data: &[u8]) -> Option<Onboarding> {
    if data.len() < 12 || data[1] != SERVICE_ID_SAMSUNG_CONNECT {
        return None;
    }
    let ocf_flags = data[3];
    let bt_mac = data
        .get(14..20)
        .filter(|_| data[13] == 0x01)
        .map(|m| m.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":"));

    // The TLV block runs to the end of the advert, so find it by its length byte.
    let (extra, tlv) = match (20..data.len()).find(|&i| i + 1 + data[i] as usize == data.len()) {
        Some(start) => (data[20..start].to_vec(), parse_tlv(&data[start + 1..])),
        None => (data.get(20..).unwrap_or_default().to_vec(), Vec::new()),
    };
    Some(Onboarding {
        packet_version: data[2],
        ocf_flags,
        owned: ocf_flags & 0x02 != 0,
        mnid: ascii(&data[5..9])?,
        setup_id: ascii(&data[9..12])?,
        bt_mac,
        extra,
        tlv,
    })
}

fn parse_tlv(mut bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    while let [kind, len, rest @ ..] = bytes {
        let Some(value) = rest.get(..*len as usize) else { break };
        out.push((*kind, value.to_vec()));
        bytes = &rest[*len as usize..];
    }
    out
}

fn ascii(bytes: &[u8]) -> Option<String> {
    bytes.iter().all(|b| b.is_ascii_graphic()).then(|| String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Built byte-for-byte the way generate_manufacturer_data() in st-device-sdk-c does.
    fn stdk_advert(owned: bool) -> Vec<u8> {
        let mut d = vec![0x42, 0x0C, 0x83, if owned { 0x03 } else { 0x05 }, 0x59];
        d.extend_from_slice(b"fTST");
        d.extend_from_slice(b"001");
        d.extend_from_slice(&[0x04, 0x01]);
        d.extend_from_slice(&[0x24, 0x0A, 0xC4, 0x11, 0x22, 0x33]);
        d.extend_from_slice(&[0x0A, 0x03, 0x08]);
        d.extend_from_slice(b"ab3fXY12");
        d
    }

    #[test]
    fn parses_stdk_onboarding_advert() {
        let onb = parse_onboarding(&stdk_advert(false)).unwrap();
        assert_eq!(onb.mnid, "fTST");
        assert_eq!(onb.setup_id, "001");
        assert!(!onb.owned);
        assert_eq!(onb.bt_mac.as_deref(), Some("24:0A:C4:11:22:33"));
        assert_eq!(onb.tlv, vec![(0x03, b"ab3fXY12".to_vec())]);
        assert!(onb.extra.is_empty());
        assert!(onb.ocf_flags_description().contains("onboarding ready"));
        assert!(parse_onboarding(&stdk_advert(true)).unwrap().owned);
    }

    #[test]
    fn parses_samsung_appliance_advert() {
        // Captured from a Samsung room air conditioner (feature 0x5d, one extra byte before the TLV).
        let d: Vec<u8> = (0..64)
            .step_by(2)
            .map(|i| {
                u8::from_str_radix(&"420c83075d30414a54303130060104bc102f4ff7ff0a02043238334804020000"[i..i + 2], 16)
                    .unwrap()
            })
            .collect();
        let onb = parse_onboarding(&d).unwrap();
        assert_eq!(onb.mnid, "0AJT");
        assert_eq!(onb.setup_id, "010");
        assert!(onb.owned);
        assert_eq!(onb.bt_mac.as_deref(), Some("04:BC:10:2F:4F:F7"));
        assert_eq!(onb.extra, vec![0xff]);
        assert_eq!(onb.tlv, vec![(0x02, b"283H".to_vec()), (0x04, vec![0, 0])]);
    }

    #[test]
    fn ignores_other_samsung_services() {
        let mut d = stdk_advert(false);
        d[1] = 0x09;
        assert!(parse_onboarding(&d).is_none());
    }

    #[test]
    fn classifies_matter_and_smarttag() {
        let mut props = PeripheralProperties::default();
        props.service_data.insert(MATTER_SERVICE, vec![0x00, 0x00, 0x0F, 0xF1, 0xFF, 0x00, 0x80, 0x00]);
        let f = classify("p1", &props, false).unwrap();
        assert_eq!(f.kind, Kind::ReadyToAdd);
        assert_eq!(f.details["matter.discriminator"], "3840");

        let mut tag = PeripheralProperties::default();
        tag.services.push(uuid16(0xFD5A));
        assert_eq!(classify("p2", &tag, false).unwrap().kind, Kind::Tracker);

        assert!(classify("p3", &PeripheralProperties::default(), false).is_none());
    }
}
