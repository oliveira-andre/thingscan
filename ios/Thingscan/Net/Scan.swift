import Foundation

/// A device found by the phone's own scan.
struct ScannedDevice: Identifiable, Hashable {
    var key: String
    var kind: Kind
    var name: String
    var address: String
    var manufacturer: String?
    var model: String?
    var ids: [String]
    var endpoints: [Endpoint]
    var sources: [String]
    var details: [String: String]
    var id: String { key }
}

enum ScanChannel: String, CaseIterable, Identifiable {
    case ocf, mdns, ble
    var id: String { rawValue }
    var label: String {
        switch self {
        case .ocf: "OCF"
        case .mdns: "Bonjour"
        case .ble: "Bluetooth"
        }
    }
}

enum ChannelState: Equatable {
    case scanning
    case done(Int)
    case failed(String)
}

/// The "Add device" scan on the phone: OCF over the subnet (the TV's API fills in its details),
/// Bonjour for hubs, Matter and Thread, and Bluetooth adverts. SSDP needs multicast, which iOS
/// only allows with an entitlement Apple grants on request, so TVs are found through OCF instead.
enum Scanner {
    static let bonjourTypes = ["_smartthings._tcp", "_smartthings-hedge._tcp", "_matterc._udp", "_matterd._udp", "_matter._tcp", "_meshcop._udp"]

    static func run(onUpdate: @escaping @MainActor (ScanChannel, ChannelState, [ScannedDevice]) -> Void) async {
        await withTaskGroup(of: Void.self) { group in
            group.addTask {
                let found = await ocf()
                await onUpdate(.ocf, .done(found.count), found)
            }
            group.addTask {
                let found = await bonjour()
                await onUpdate(.mdns, .done(found.count), found)
            }
            group.addTask {
                switch await BLEScanner.scan(for: 6) {
                case let .success(sightings):
                    let found = sightings.compactMap(classify)
                    await onUpdate(.ble, .done(found.count), found)
                case let .failure(error):
                    await onUpdate(.ble, .failed(error.localizedDescription), [])
                }
            }
        }
    }

    static func ocf() async -> [ScannedDevice] {
        let found = await OCF.discover()
        return await withTaskGroup(of: ScannedDevice.self) { group in
            for device in found {
                group.addTask {
                    let kind: Kind = device.deviceTypes.contains("oic.d.tv") ? .tv
                        : device.deviceTypes.contains("oic.d.bridge") ? .hub
                        : device.deviceTypes.isEmpty ? .other : .appliance
                    var scanned = ScannedDevice(
                        key: device.di.map { "ocf:\($0)" } ?? device.ip, kind: kind,
                        name: device.name ?? device.deviceTypes.first.map { "OCF \($0.replacingOccurrences(of: "oic.d.", with: ""))" } ?? "OCF device",
                        address: device.ip, manufacturer: device.manufacturer, model: device.model,
                        ids: device.di.map { ["ocf:\($0)"] } ?? [], endpoints: [.ocf(ip: device.ip, di: device.di)],
                        sources: ["ocf"], details: ["ocf.device_types": device.deviceTypes.joined(separator: ",")]
                    )
                    if kind == .tv, let tv = await SamsungTV.info(ip: device.ip) {
                        scanned.name = tv.name ?? scanned.name
                        scanned.model = tv.model ?? scanned.model
                        if let id = tv.id { scanned.ids.append("samsung-tv:\(id)") }
                        scanned.endpoints.append(.samsungTv(ip: device.ip, id: tv.id, mac: tv.mac))
                        if let mac = tv.mac { scanned.details["tv.wifi_mac"] = mac }
                    }
                    return scanned
                }
            }
            var out: [ScannedDevice] = []
            for await device in group { out.append(device) }
            return out
        }
    }

    static func bonjour() async -> [ScannedDevice] {
        await withTaskGroup(of: [ScannedDevice].self) { group in
            for type in bonjourTypes {
                group.addTask {
                    await Bonjour.browse(type, for: 4).map { service in
                        let kind: Kind = switch type {
                        case "_matterc._udp": service.txt["CM"] == "0" ? .matter : .readyToAdd
                        case "_matter._tcp": .matter
                        case "_meshcop._udp": .threadBorderRouter
                        default: .hub
                        }
                        let name = service.txt["DN"]
                            ?? [service.txt["vn"], service.txt["mn"]].compactMap { $0 }.joined(separator: " ").nilIfEmpty
                            ?? service.name
                        return ScannedDevice(
                            key: "mdns:\(service.fullname)", kind: kind, name: name, address: service.name,
                            ids: ["mdns:\(service.fullname)"], endpoints: [.mdns(service: "\(type).local.", fullname: service.fullname)],
                            sources: ["mdns"], details: service.txt.mapKeys { "txt.\($0)" }
                        )
                    }
                }
            }
            var out: [ScannedDevice] = []
            for await devices in group { out += devices }
            return out
        }
    }

    /// Samsung / SmartThings / Matter adverts only; everything else is noise for adding devices.
    static func classify(_ s: BLESighting) -> ScannedDevice? {
        var details: [String: String] = ["rssi": "\(s.rssi) dBm"]
        let kind: Kind
        var name = s.name
        var manufacturer: String?
        if let onboarding = s.onboarding {
            kind = onboarding.owned ? .samsung : .readyToAdd
            name = name ?? "SmartThings device \(onboarding.mnid)"
            details["st.mnid"] = onboarding.mnid
            details["st.setup_id"] = onboarding.setupId
            if let mac = onboarding.btMac { details["st.bt_mac"] = mac }
        } else if let matter = s.serviceData["FFF6"], matter.count >= 7 {
            kind = .readyToAdd
            let discriminator = (UInt16(matter[1]) | UInt16(matter[2]) << 8) & 0x0FFF
            name = name ?? "Matter device \(discriminator)"
            details["matter.discriminator"] = "\(discriminator)"
        } else if s.services.contains("FD5A") || s.serviceData["FD5A"] != nil {
            kind = .tracker
            name = name ?? "Galaxy SmartTag"
            manufacturer = "Samsung"
        } else if s.manufacturerData[0x0075] != nil {
            kind = .samsung
            name = name ?? "Samsung BLE device"
            manufacturer = "Samsung"
        } else {
            return nil
        }
        return ScannedDevice(
            key: "ble:\(s.uuid)", kind: kind, name: name ?? "Bluetooth device", address: s.uuid,
            manufacturer: manufacturer, ids: s.ids, endpoints: [.ble(id: s.uuid)], sources: ["ble"], details: details
        )
    }
}

extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

extension Dictionary {
    func mapKeys<K: Hashable>(_ transform: (Key) -> K) -> [K: Value] {
        Dictionary<K, Value>(uniqueKeysWithValues: map { (transform($0.key), $0.value) })
    }
}
