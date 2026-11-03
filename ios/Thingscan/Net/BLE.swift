import CoreBluetooth
import Foundation

/// One Bluetooth advertiser heard during a scan.
struct BLESighting {
    var uuid: String
    var name: String?
    var rssi: Int
    var manufacturerData: [UInt16: [UInt8]]
    var serviceData: [String: [UInt8]]
    var services: [String]

    /// SmartThings easy-setup advert (Samsung company 0x0075, service 0x0C), as the Mac reads it.
    var onboarding: (mnid: String, setupId: String, owned: Bool, btMac: String?)? {
        guard let data = manufacturerData[0x0075], data.count >= 12, data[1] == 0x0C else { return nil }
        let ascii = { (bytes: ArraySlice<UInt8>) -> String? in
            bytes.allSatisfy { $0 > 0x20 && $0 < 0x7F } ? String(decoding: bytes, as: UTF8.self) : nil
        }
        guard let mnid = ascii(data[5 ..< 9]), let setup = ascii(data[9 ..< 12]) else { return nil }
        let mac = data.count >= 20 && data[13] == 0x01 ? data[14 ..< 20].map { String(format: "%02X", $0) }.joined(separator: ":") : nil
        return (mnid, setup, data[3] & 0x02 != 0, mac)
    }

    /// Ids this sighting answers to: the peripheral id on this phone, and the Bluetooth MAC
    /// when the advert carries it (peripheral ids differ between the Mac and the phone).
    var ids: [String] {
        var ids = ["ble:\(uuid)"]
        if let mac = onboarding?.btMac { ids.append("btmac:\(mac)") }
        return ids
    }
}

/// Listens for Bluetooth adverts for a while. Unavailable in the simulator. Only ever touched
/// on the main queue.
final class BLEScanner: NSObject, CBCentralManagerDelegate, @unchecked Sendable {
    private var central: CBCentralManager?
    private var found: [String: BLESighting] = [:]
    private var continuation: CheckedContinuation<Result<[BLESighting], BLEError>, Never>?
    private var duration: TimeInterval = 4

    enum BLEError: LocalizedError {
        case unavailable(String)
        var errorDescription: String? { if case let .unavailable(m) = self { m } else { nil } }
    }

    static func scan(for seconds: TimeInterval) async -> Result<[BLESighting], BLEError> {
        let scanner = BLEScanner()
        return await withCheckedContinuation { continuation in
            DispatchQueue.main.async {
                scanner.continuation = continuation
                scanner.duration = seconds
                scanner.central = CBCentralManager(delegate: scanner, queue: .main)
                // Bluetooth that never becomes ready (e.g. permission pending) still ends the scan.
                DispatchQueue.main.asyncAfter(deadline: .now() + seconds + 3) { scanner.finish() }
            }
        }
    }

    func centralManagerDidUpdateState(_ central: CBCentralManager) {
        switch central.state {
        case .poweredOn:
            central.scanForPeripherals(withServices: nil, options: [CBCentralManagerScanOptionAllowDuplicatesKey: false])
            DispatchQueue.main.asyncAfter(deadline: .now() + duration) { self.finish() }
        case .unauthorized:
            finish(error: "Bluetooth isn't allowed for Thingscan (Settings › Thingscan › Bluetooth).")
        case .poweredOff:
            finish(error: "Bluetooth is off.")
        case .unsupported:
            finish(error: "Bluetooth isn't available on this device.")
        default:
            break
        }
    }

    func centralManager(_ central: CBCentralManager, didDiscover peripheral: CBPeripheral, advertisementData: [String: Any], rssi RSSI: NSNumber) {
        let uuid = peripheral.identifier.uuidString.lowercased()
        var manufacturer: [UInt16: [UInt8]] = [:]
        if let data = advertisementData[CBAdvertisementDataManufacturerDataKey] as? Data, data.count >= 2 {
            let bytes = [UInt8](data)
            manufacturer[UInt16(bytes[0]) | UInt16(bytes[1]) << 8] = Array(bytes[2...])
        }
        var service: [String: [UInt8]] = [:]
        if let data = advertisementData[CBAdvertisementDataServiceDataKey] as? [CBUUID: Data] {
            for (key, value) in data { service[key.uuidString.uppercased()] = [UInt8](value) }
        }
        let services = (advertisementData[CBAdvertisementDataServiceUUIDsKey] as? [CBUUID])?.map { $0.uuidString.uppercased() } ?? []
        let name = advertisementData[CBAdvertisementDataLocalNameKey] as? String ?? peripheral.name
        var sighting = found[uuid] ?? BLESighting(uuid: uuid, name: name, rssi: RSSI.intValue, manufacturerData: [:], serviceData: [:], services: [])
        sighting.name = name ?? sighting.name
        sighting.rssi = RSSI.intValue
        sighting.manufacturerData.merge(manufacturer) { $1 }
        sighting.serviceData.merge(service) { $1 }
        sighting.services = Array(Set(sighting.services + services))
        found[uuid] = sighting
    }

    private func finish(error: String? = nil) {
        guard let continuation else { return }
        self.continuation = nil
        central?.stopScan()
        central = nil
        continuation.resume(returning: error.map { .failure(.unavailable($0)) } ?? .success(Array(found.values)))
    }
}
