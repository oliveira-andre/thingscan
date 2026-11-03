import Foundation

/// OCF over CoAP, as Samsung TVs and appliances speak it on the LAN. iOS doesn't allow
/// multicast without a special entitlement, so discovery asks every host on the subnet
/// directly, which is also the only way the user's devices answer.
enum OCF {
    static let port: UInt16 = 5683

    struct Found {
        var ip: String
        var di: String?
        var name: String?
        var manufacturer: String?
        var model: String?
        var deviceTypes: [String]
        var hrefs: [String]
    }

    enum PingResult {
        case alive(String)
        case different(String)
        case silent
    }

    /// GET /oic/d on the well-known port: any answer means the device is up; a different
    /// `di` means another device now holds the address.
    static func ping(ip: String, di: String?, wait: TimeInterval = 1.5) async -> PingResult {
        await blocking {
            guard let socket = UDPSocket() else { return .silent }
            let request = CoAPMessage.get("/oic/d", messageId: MessageIds.take(), token: [0x0D, 0x02])
            guard let reply = exchange(socket, ip: ip, port: port, request: request, wait: wait) else { return .silent }
            guard reply.isSuccess else { return .alive("CoAP \(reply.codeString)") }
            let device = CBOR.decode(reply.payload)
            if let want = di, let got = device?["di"]?.text, got != want {
                return .different("a different OCF device (\(got)) answers here now")
            }
            return .alive(device?["n"]?.text ?? "OCF")
        }
    }

    /// Asks every host on the subnet for /oic/res, then reads each responder's /oic/d and /oic/p.
    static func discover(listen: TimeInterval = 3) async -> [Found] {
        let hosts = LocalNetwork.subnetHosts()
        guard !hosts.isEmpty else { return [] }
        let responders: [(ip: String, port: UInt16, info: ResInfo)] = await blocking {
            guard let socket = UDPSocket() else { return [] }
            let probe = CoAPMessage.get("/oic/res", messageId: 0, token: [0x0C, 0xF1])
            for host in hosts {
                var message = probe
                message.messageId = MessageIds.take()
                socket.send(message.encode(), to: host, port: port)
            }
            // First successful reply per responder.
            var firsts: [(ip: String, port: UInt16, message: CoAPMessage)] = []
            let deadline = Date().addingTimeInterval(listen)
            while Date() < deadline, let datagram = socket.receive(timeout: deadline.timeIntervalSinceNow) {
                guard let message = CoAPMessage.decode(datagram.data), message.isSuccess,
                      !firsts.contains(where: { $0.ip == datagram.ip && $0.port == datagram.port }) else { continue }
                firsts.append((datagram.ip, datagram.port, message))
            }
            // Large documents come block-wise; follow-ups must use this same socket.
            return firsts.compactMap { first in
                guard let payload = complete(socket, ip: first.ip, port: first.port, template: probe, first: first.message, wait: 1.2),
                      let info = parseRes(payload) else { return nil }
                return (first.ip, first.port, info)
            }
        }
        var seen = Set<String>()
        var found: [Found] = []
        for responder in responders {
            let key = responder.info.di ?? responder.ip
            guard seen.insert(key).inserted else { continue }
            async let device = resource(ip: responder.ip, path: "/oic/d")
            async let platform = resource(ip: responder.ip, path: "/oic/p")
            let (d, p) = await (device, platform)
            let model = p?["mnmo"]?.text.map { String($0.split(separator: "|").first ?? Substring($0)) }
            found.append(Found(
                ip: responder.ip, di: responder.info.di, name: d?["n"]?.text, manufacturer: p?["mnmn"]?.text,
                model: model, deviceTypes: responder.info.resourceTypes.filter { $0.hasPrefix("oic.d.") },
                hrefs: responder.info.hrefs
            ))
        }
        return found
    }

    static func resource(ip: String, path: String, wait: TimeInterval = 1.2) async -> CBOR? {
        await blocking {
            guard let socket = UDPSocket() else { return nil }
            let request = CoAPMessage.get(path, messageId: MessageIds.take(), token: [0x0D, 0x01])
            guard let first = exchange(socket, ip: ip, port: port, request: request, wait: wait), first.isSuccess,
                  let payload = complete(socket, ip: ip, port: port, template: request, first: first, wait: wait) else { return nil }
            return CBOR.decode(payload)
        }
    }

    /// Sends `request` and waits for the reply with its token from `ip` (any port).
    private static func exchange(_ socket: UDPSocket, ip: String, port: UInt16, request: CoAPMessage, wait: TimeInterval, block: UInt32? = nil) -> CoAPMessage? {
        socket.send(request.encode(), to: ip, port: port)
        let deadline = Date().addingTimeInterval(wait)
        while Date() < deadline, let datagram = socket.receive(timeout: deadline.timeIntervalSinceNow) {
            guard datagram.ip == ip, let reply = CoAPMessage.decode(datagram.data), reply.token == request.token else { continue }
            if let block, reply.block?.num != block { continue }
            return reply
        }
        return nil
    }

    /// Follows block-wise transfer from an already-received first block.
    private static func complete(_ socket: UDPSocket, ip: String, port: UInt16, template: CoAPMessage, first: CoAPMessage, wait: TimeInterval) -> [UInt8]? {
        var payload = first.payload
        var block = first.block
        for _ in 0 ..< 128 {
            guard let current = block, current.more else { return payload }
            let num = UInt32(payload.count >> (Int(current.szx) + 4))
            let request = template.forBlock(messageId: MessageIds.take(), num: num, szx: current.szx)
            guard let reply = exchange(socket, ip: ip, port: port, request: request, wait: wait, block: num), reply.isSuccess else { return nil }
            payload += reply.payload
            block = reply.block
        }
        return nil
    }

    struct ResInfo {
        var di: String?
        var resourceTypes: [String] = []
        var hrefs: [String] = []
    }

    /// OIC 1.1 (`[{di, links}]`), OCF 1.x link lists (`[{anchor: "ocf://<di>", ...}]`) and baseline maps.
    static func parseRes(_ payload: [UInt8]) -> ResInfo? {
        guard let value = CBOR.decode(payload) else { return nil }
        var info = ResInfo()
        let top: [CBOR]
        switch value {
        case let .array(items): top = items
        case .map: top = [value]
        default: return nil
        }
        var links: [CBOR] = []
        for item in top {
            if info.di == nil { info.di = item["di"]?.text }
            if let inner = item["links"]?.array { links += inner } else { links.append(item) }
        }
        for link in links {
            if info.di == nil, let anchor = link["anchor"]?.text, anchor.hasPrefix("ocf://") {
                info.di = String(anchor.dropFirst(6)).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            }
            if let href = link["href"]?.text { info.hrefs.append(href) }
            info.resourceTypes += link["rt"]?.texts ?? []
        }
        return info.di != nil || !info.hrefs.isEmpty ? info : nil
    }
}
