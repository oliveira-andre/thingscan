import Darwin
import Foundation

/// An unconnected IPv4 UDP socket. Samsung devices answer CoAP from a different port than
/// they were asked on, which a connected socket (or `NWConnection`) would drop.
final class UDPSocket {
    private let fd: Int32

    init?() {
        fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP)
        guard fd >= 0 else { return nil }
        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_addr.s_addr = INADDR_ANY
        address.sin_port = 0
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        if bound != 0 {
            close(fd)
            return nil
        }
    }

    deinit { close(fd) }

    /// Lets this socket send to broadcast addresses. iOS still refuses those sends unless the
    /// app has Apple's multicast entitlement.
    func allowBroadcast() {
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_BROADCAST, &on, socklen_t(MemoryLayout<Int32>.size))
    }

    @discardableResult
    func send(_ data: Data, to ip: String, port: UInt16) -> Bool {
        var address = sockaddr_in()
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        guard inet_pton(AF_INET, ip, &address.sin_addr) == 1 else { return false }
        let sent = data.withUnsafeBytes { bytes in
            withUnsafePointer(to: &address) {
                $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    sendto(fd, bytes.baseAddress, data.count, 0, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
                }
            }
        }
        return sent == data.count
    }

    /// Waits up to `timeout` seconds for one datagram.
    func receive(timeout: TimeInterval) -> (data: Data, ip: String, port: UInt16)? {
        var poller = pollfd(fd: fd, events: Int16(POLLIN), revents: 0)
        guard poll(&poller, 1, Int32(max(0, timeout) * 1000)) > 0 else { return nil }
        var buffer = [UInt8](repeating: 0, count: 9000)
        var from = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let count = withUnsafeMutablePointer(to: &from) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { recvfrom(fd, &buffer, buffer.count, 0, $0, &length) }
        }
        guard count > 0 else { return nil }
        var text = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
        inet_ntop(AF_INET, &from.sin_addr, &text, socklen_t(INET_ADDRSTRLEN))
        return (Data(buffer[..<count]), String(cString: text), UInt16(bigEndian: from.sin_port))
    }
}

/// Runs blocking socket work off the cooperative thread pool.
func blocking<T>(_ work: @escaping () -> T) async -> T {
    await withCheckedContinuation { continuation in
        DispatchQueue.global(qos: .userInitiated).async { continuation.resume(returning: work()) }
    }
}

enum LocalNetwork {
    /// The phone's Wi-Fi IPv4 address and prefix length.
    static func wifi() -> (ip: String, prefix: Int)? {
        var head: UnsafeMutablePointer<ifaddrs>?
        guard getifaddrs(&head) == 0, let first = head else { return nil }
        defer { freeifaddrs(head) }
        var best: (ip: String, prefix: Int)?
        for pointer in sequence(first: first, next: { $0.pointee.ifa_next }) {
            let entry = pointer.pointee
            guard let address = entry.ifa_addr, address.pointee.sa_family == sa_family_t(AF_INET),
                  let mask = entry.ifa_netmask else { continue }
            let name = String(cString: entry.ifa_name)
            guard name.hasPrefix("en") else { continue }
            let ip = address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee.sin_addr }
            let netmask = mask.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { $0.pointee.sin_addr.s_addr }
            var text = [CChar](repeating: 0, count: Int(INET_ADDRSTRLEN))
            var copy = ip
            inet_ntop(AF_INET, &copy, &text, socklen_t(INET_ADDRSTRLEN))
            let string = String(cString: text)
            if string.hasPrefix("169.254.") { continue }
            let prefix = UInt32(bigEndian: netmask).nonzeroBitCount
            if best == nil || name == "en0" { best = (string, prefix) }
        }
        return best
    }

    /// The Wi-Fi subnet's broadcast address, e.g. 192.168.3.255.
    static func broadcast() -> String? {
        guard let (ip, prefix) = wifi(), (1...30).contains(prefix) else { return nil }
        var address = in_addr()
        inet_pton(AF_INET, ip, &address)
        let host = UInt32(bigEndian: address.s_addr) | ~(UInt32.max << (32 - UInt32(prefix)))
        return "\(host >> 24).\((host >> 16) & 0xFF).\((host >> 8) & 0xFF).\(host & 0xFF)"
    }

    /// Every other host on the phone's subnet, or nothing when it's bigger than a /22.
    static func subnetHosts() -> [String] {
        guard let (ip, prefix) = wifi(), (22...30).contains(prefix) else { return [] }
        var address = in_addr()
        inet_pton(AF_INET, ip, &address)
        let own = UInt32(bigEndian: address.s_addr)
        let mask = UInt32.max << (32 - UInt32(prefix))
        let network = own & mask
        let broadcast = network | ~mask
        return (network + 1 ..< broadcast).filter { $0 != own }.map { host in
            "\(host >> 24).\((host >> 16) & 0xFF).\((host >> 8) & 0xFF).\(host & 0xFF)"
        }
    }
}
