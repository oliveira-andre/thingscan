import Foundation

/// Checks whether saved devices still answer, using the endpoints recorded when they were
/// added (the same rules as the Mac's reach.rs). A LAN device that went quiet is looked for
/// again by its OCF id, in case it came back under a new address.
enum Reach {
    static func check(
        _ devices: [SavedDevice],
        cloud: SmartThingsClient?,
        onReport: @escaping @MainActor (Report, [Endpoint]?) -> Void
    ) async {
        let needsBLE = devices.contains { device in
            device.endpoints.contains { if case .ble = $0 { true } else { false } }
        }
        let ble = Task { needsBLE ? await BLEScanner.scan(for: 4) : .success([]) }
        let types = Set(devices.flatMap(\.endpoints).compactMap { if case let .mdns(service, _) = $0 { service } else { nil } })
        let mdns = Task {
            await withTaskGroup(of: [Bonjour.Service].self) { group in
                for type in types { group.addTask { await Bonjour.browse(type, for: 3) } }
                var all = Set<String>()
                for await services in group { all.formUnion(services.map(\.fullname)) }
                return all
            }
        }

        var lost: [SavedDevice] = []
        await withTaskGroup(of: (SavedDevice, Report).self) { group in
            for device in devices {
                group.addTask {
                    var probes: [Probe] = []
                    await withTaskGroup(of: Probe?.self) { inner in
                        for endpoint in device.endpoints {
                            inner.addTask { await probe(endpoint, device: device, cloud: cloud, ble: ble, mdns: mdns) }
                        }
                        for await probe in inner { if let probe { probes.append(probe) } }
                    }
                    let status = summarize(probes)
                    return (device, Report(id: device.id, status: status, address: probes.first { $0.status == .reachable }?.target, probes: probes))
                }
            }
            for await (device, report) in group {
                if report.status == .unreachable, device.ids.contains(where: { $0.hasPrefix("ocf:") }) {
                    lost.append(device)
                } else {
                    await onReport(report, nil)
                }
            }
        }

        guard !lost.isEmpty else { return }
        let found = await OCF.discover()
        for device in lost {
            var report = Report(id: device.id, status: .unreachable, address: nil, probes: [])
            if let match = found.first(where: { $0.di.map { device.ids.contains("ocf:\($0)") } ?? false }) {
                report.status = .reachable
                report.address = match.ip
                report.probes = [Probe(via: "OCF", target: match.ip, status: .reachable, detail: "found again at a new address", latencyMs: nil)]
                var endpoints = device.endpoints.filter { if case .ocf = $0 { false } else { true } }
                endpoints.append(.ocf(ip: match.ip, di: match.di))
                await onReport(report, endpoints)
            } else {
                for endpoint in device.endpoints {
                    if let probe = await probe(endpoint, device: device, cloud: cloud, ble: ble, mdns: mdns) { report.probes.append(probe) }
                }
                report.status = summarize(report.probes)
                await onReport(report, nil)
            }
        }
    }

    static func summarize(_ probes: [Probe]) -> ReachStatus {
        if probes.contains(where: { $0.status == .reachable }) { return .reachable }
        if probes.contains(where: { $0.status == .unreachable }) { return .unreachable }
        return .unknown
    }

    private static func probe(
        _ endpoint: Endpoint,
        device: SavedDevice,
        cloud: SmartThingsClient?,
        ble: Task<Result<[BLESighting], BLEScanner.BLEError>, Never>,
        mdns: Task<Set<String>, Never>
    ) async -> Probe? {
        let started = Date()
        let ms = { Int(Date().timeIntervalSince(started) * 1000) }
        switch endpoint {
        case let .ocf(ip, di):
            switch await OCF.ping(ip: ip, di: di) {
            case let .alive(detail): return Probe(via: "OCF", target: ip, status: .reachable, detail: detail, latencyMs: ms())
            case let .different(detail): return Probe(via: "OCF", target: ip, status: .unreachable, detail: detail, latencyMs: nil)
            case .silent: return Probe(via: "OCF", target: ip, status: .unreachable, detail: "no CoAP reply", latencyMs: nil)
            }
        case let .upnp(location, udn):
            let host = URL(string: location)?.host ?? location
            guard let url = URL(string: location) else { return nil }
            var request = URLRequest(url: url)
            request.timeoutInterval = 1.5
            guard let (data, _) = try? await URLSession.shared.data(for: request) else {
                return Probe(via: "UPnP", target: host, status: .unreachable, detail: "no answer", latencyMs: nil)
            }
            let xml = String(decoding: data, as: UTF8.self)
            if let udn, let got = tag("UDN", in: xml), got != udn {
                return Probe(via: "UPnP", target: host, status: .unreachable, detail: "a different device (\(got)) answers here now", latencyMs: nil)
            }
            return Probe(via: "UPnP", target: host, status: .reachable, detail: tag("friendlyName", in: xml) ?? "UPnP description", latencyMs: ms())
        case let .samsungTv(ip, id, _):
            guard let info = await SamsungTV.info(ip: ip, timeout: 1.5) else {
                return Probe(via: "TV API", target: ip, status: .unreachable, detail: "no answer on :8001", latencyMs: nil)
            }
            if let id, let got = info.id, got != id {
                return Probe(via: "TV API", target: ip, status: .unreachable, detail: "a different TV answers here now", latencyMs: nil)
            }
            return Probe(via: "TV API", target: ip, status: .reachable, detail: info.power.map { "power \($0)" } ?? "TV API", latencyMs: ms())
        case let .mdns(_, fullname):
            let seen = await mdns.value
            return Probe(via: "mDNS", target: fullname, status: seen.contains(fullname) ? .reachable : .unreachable,
                         detail: seen.contains(fullname) ? "advertising" : "not advertising", latencyMs: nil)
        case let .ble(id):
            switch await ble.value {
            case let .failure(error):
                return Probe(via: "BLE", target: id, status: .unknown, detail: error.localizedDescription, latencyMs: nil)
            case let .success(sightings):
                if let match = sightings.first(where: { device.sharesIdentity(with: $0.ids) }) {
                    return Probe(via: "BLE", target: match.uuid, status: .reachable, detail: "\(match.rssi) dBm", latencyMs: nil)
                }
                // Bluetooth ids differ between the Mac and the phone; only adverts carrying the
                // MAC can be matched on a device added on the Mac.
                if device.syncedFromMac, !device.ids.contains(where: { $0.hasPrefix("btmac:") }) {
                    return Probe(via: "BLE", target: id, status: .unknown, detail: "only the Mac can recognise this one", latencyMs: nil)
                }
                return Probe(via: "BLE", target: id, status: .unreachable, detail: "not advertising", latencyMs: nil)
            }
        case let .softAp(ssid):
            return Probe(via: "Wi-Fi", target: ssid, status: .unknown, detail: "setup networks aren't checked", latencyMs: nil)
        case let .cloud(deviceId):
            guard let cloud else {
                return Probe(via: "SmartThings", target: deviceId, status: .unknown, detail: "SmartThings isn't connected", latencyMs: nil)
            }
            do {
                let state = try await cloud.health(deviceId)
                let online = state == "ONLINE"
                return Probe(via: "SmartThings", target: deviceId, status: online ? .reachable : .unreachable,
                             detail: online ? "online in SmartThings" : "\(state.lowercased()) in SmartThings", latencyMs: online ? ms() : nil)
            } catch {
                return Probe(via: "SmartThings", target: deviceId, status: .unknown, detail: error.localizedDescription, latencyMs: nil)
            }
        }
    }

    /// The text of the first `<name>` element, enough for UPnP descriptions.
    static func tag(_ name: String, in xml: String) -> String? {
        guard let open = xml.range(of: "<\(name)>"), let close = xml.range(of: "</\(name)>", range: open.upperBound ..< xml.endIndex) else { return nil }
        return String(xml[open.upperBound ..< close.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
