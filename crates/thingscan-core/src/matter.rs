//! Matter identifiers shared by the mDNS (`_matterc._udp`) and BLE (UUID 0xFFF6) scanners.

/// Device type IDs from the Matter Device Library (common subset).
pub fn device_type_name(id: u32) -> Option<&'static str> {
    Some(match id {
        0x000A => "Door Lock",
        0x000B => "Door Lock Controller",
        0x000E => "Bridge",
        0x000F => "Generic Switch",
        0x0015 => "Contact Sensor",
        0x0016 => "Root Node",
        0x0022 => "Speaker",
        0x0023 => "Casting Video Player",
        0x0028 => "Basic Video Player",
        0x002B => "Fan",
        0x002C => "Air Quality Sensor",
        0x002D => "Air Purifier",
        0x0042 => "Water Valve",
        0x0043 => "Water Leak Detector",
        0x0070 => "Refrigerator",
        0x0072 => "Room Air Conditioner",
        0x0073 => "Laundry Washer",
        0x0074 => "Robotic Vacuum Cleaner",
        0x0075 => "Dishwasher",
        0x0076 => "Smoke CO Alarm",
        0x007C => "Laundry Dryer",
        0x0100 => "On/Off Light",
        0x0101 => "Dimmable Light",
        0x0103 => "On/Off Light Switch",
        0x0104 => "Dimmer Switch",
        0x0105 => "Color Dimmer Switch",
        0x0106 => "Light Sensor",
        0x0107 => "Occupancy Sensor",
        0x010A => "On/Off Plug-in Unit",
        0x010B => "Dimmable Plug-in Unit",
        0x010C => "Color Temperature Light",
        0x010D => "Extended Color Light",
        0x0202 => "Window Covering",
        0x0301 => "Thermostat",
        0x0302 => "Temperature Sensor",
        0x0303 => "Pump",
        0x0305 => "Pressure Sensor",
        0x0306 => "Flow Sensor",
        0x0307 => "Humidity Sensor",
        _ => return None,
    })
}

pub fn commissioning_mode(cm: &str) -> &'static str {
    match cm {
        "0" => "not commissionable",
        "1" => "commissionable (basic)",
        "2" => "commissionable (enhanced)",
        _ => "unknown",
    }
}

/// Matter BLE commissionable advertisement, carried as service data for UUID 0xFFF6:
/// opcode(1) | version:4 + discriminator:12 (LE u16) | vendor id (LE u16) | product id (LE u16) | flags(1)
#[derive(Debug, PartialEq, Eq)]
pub struct BleAdvert {
    pub opcode: u8,
    pub version: u8,
    pub discriminator: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub additional_data: bool,
}

pub fn parse_ble_service_data(data: &[u8]) -> Option<BleAdvert> {
    if data.len() < 7 {
        return None;
    }
    let vd = u16::from_le_bytes([data[1], data[2]]);
    Some(BleAdvert {
        opcode: data[0],
        version: (vd >> 12) as u8,
        discriminator: vd & 0x0FFF,
        vendor_id: u16::from_le_bytes([data[3], data[4]]),
        product_id: u16::from_le_bytes([data[5], data[6]]),
        additional_data: data.get(7).is_some_and(|f| f & 0x01 != 0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ble_commissionable_advert() {
        // discriminator 0xF00, version 0, VID 0xFFF1, PID 0x8000
        let adv = parse_ble_service_data(&[0x00, 0x00, 0x0F, 0xF1, 0xFF, 0x00, 0x80, 0x00]).unwrap();
        assert_eq!(adv.discriminator, 0xF00);
        assert_eq!(adv.version, 0);
        assert_eq!(adv.vendor_id, 0xFFF1);
        assert_eq!(adv.product_id, 0x8000);
        assert!(!adv.additional_data);
    }
}
