import Foundation

/// Wake-on-LAN, which turns on a TV with "Power on with mobile" enabled.
///
/// iOS only lets apps broadcast with Apple's multicast entitlement, which a free team can't
/// get (the SmartThings app has it). So the packet goes straight to the TV's address too; that
/// reaches the TV while the phone still remembers its hardware address from talking to it.
enum WakeOnLAN {
    /// Six 0xFF bytes, then the MAC sixteen times.
    static func packet(mac: String) -> Data? {
        let parts = mac.split(whereSeparator: { $0 == ":" || $0 == "-" })
        guard parts.count == 6 else { return nil }
        var bytes: [UInt8] = []
        for part in parts {
            guard part.count <= 2, let byte = UInt8(part, radix: 16) else { return nil }
            bytes.append(byte)
        }
        return Data(repeating: 0xFF, count: 6) + Data((0 ..< 16).flatMap { _ in bytes })
    }

    enum Failure: LocalizedError {
        case badMac, notSent

        var errorDescription: String? {
            switch self {
            case .badMac: "the TV's MAC address isn't valid"
            case .notSent: "the wake-up packet couldn't be sent"
            }
        }
    }

    /// Sends the packet to the TV's address and, where iOS allows it, the broadcast addresses.
    /// Fails when no copy left the phone.
    static func send(mac: String, ip: String?) async throws {
        guard let packet = packet(mac: mac) else { throw Failure.badMac }
        let sent = await blocking {
            guard let socket = UDPSocket() else { return false }
            socket.allowBroadcast()
            let targets = [ip, LocalNetwork.broadcast(), "255.255.255.255"].compactMap { $0 }
            var sent = false
            for target in targets {
                for port: UInt16 in [9, 7] {
                    sent = socket.send(packet, to: target, port: port) || sent
                }
            }
            return sent
        }
        if !sent { throw Failure.notSent }
    }
}
