import Foundation

/// Just enough CoAP (RFC 7252, block-wise transfer from RFC 7959) for OCF discovery and pings.
struct CoAPMessage {
    static let uriPath: UInt16 = 11
    static let block2: UInt16 = 23

    var type: UInt8 = 1 // non-confirmable
    var code: UInt8 = 0x01 // GET
    var messageId: UInt16
    var token: [UInt8]
    var options: [(UInt16, [UInt8])] = []
    var payload: [UInt8] = []

    static func get(_ path: String, messageId: UInt16, token: [UInt8]) -> CoAPMessage {
        var message = CoAPMessage(messageId: messageId, token: token)
        message.options = path.split(separator: "/").map { (uriPath, Array($0.utf8)) }
        return message
    }

    var isSuccess: Bool { code >> 5 == 2 }
    var codeString: String { "\(code >> 5).\(String(format: "%02d", code & 0x1F))" }

    /// (number, more follows, size exponent)
    var block: (num: UInt32, more: Bool, szx: UInt8)? {
        guard let value = options.first(where: { $0.0 == CoAPMessage.block2 })?.1 else { return nil }
        let v = value.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        return (v >> 4, v & 0x08 != 0, UInt8(v & 0x07))
    }

    func forBlock(messageId: UInt16, num: UInt32, szx: UInt8) -> CoAPMessage {
        var copy = self
        copy.messageId = messageId
        copy.options.removeAll { $0.0 == CoAPMessage.block2 }
        let v = (num << 4) | UInt32(szx)
        let bytes: [UInt8] = v < 0x100 ? [UInt8(v)] : v < 0x10000 ? [UInt8(v >> 8), UInt8(v & 0xFF)] : [UInt8(v >> 16), UInt8((v >> 8) & 0xFF), UInt8(v & 0xFF)]
        copy.options.append((CoAPMessage.block2, bytes))
        return copy
    }

    func encode() -> Data {
        var out: [UInt8] = [0x40 | (type << 4) | UInt8(token.count), code, UInt8(messageId >> 8), UInt8(messageId & 0xFF)]
        out += token
        var last: UInt16 = 0
        for (number, value) in options.sorted(by: { $0.0 < $1.0 }) {
            let (deltaNibble, deltaExt) = Self.nibble(number - last)
            let (lengthNibble, lengthExt) = Self.nibble(UInt16(value.count))
            out.append((deltaNibble << 4) | lengthNibble)
            out += deltaExt + lengthExt + value
            last = number
        }
        if !payload.isEmpty { out += [0xFF] + payload }
        return Data(out)
    }

    static func decode(_ data: Data) -> CoAPMessage? {
        let bytes = [UInt8](data)
        guard bytes.count >= 4, bytes[0] >> 6 == 1 else { return nil }
        let tokenLength = Int(bytes[0] & 0x0F)
        guard bytes.count >= 4 + tokenLength else { return nil }
        var message = CoAPMessage(
            type: (bytes[0] >> 4) & 0x3, code: bytes[1], messageId: UInt16(bytes[2]) << 8 | UInt16(bytes[3]),
            token: Array(bytes[4 ..< 4 + tokenLength])
        )
        var i = 4 + tokenLength
        var number: UInt16 = 0
        while i < bytes.count {
            if bytes[i] == 0xFF { i += 1; break }
            let header = bytes[i]
            i += 1
            guard let delta = readExtended(header >> 4, bytes, &i), let length = readExtended(header & 0x0F, bytes, &i) else { return nil }
            number &+= delta
            guard i + Int(length) <= bytes.count else { return nil }
            message.options.append((number, Array(bytes[i ..< i + Int(length)])))
            i += Int(length)
        }
        message.payload = i < bytes.count ? Array(bytes[i...]) : []
        return message
    }

    private static func nibble(_ v: UInt16) -> (UInt8, [UInt8]) {
        switch v {
        case 0 ... 12: (UInt8(v), [])
        case 13 ... 268: (13, [UInt8(v - 13)])
        default: (14, [UInt8((v - 269) >> 8), UInt8((v - 269) & 0xFF)])
        }
    }

    private static func readExtended(_ nibble: UInt8, _ bytes: [UInt8], _ i: inout Int) -> UInt16? {
        switch nibble {
        case 0 ... 12: return UInt16(nibble)
        case 13:
            guard i < bytes.count else { return nil }
            defer { i += 1 }
            return UInt16(bytes[i]) + 13
        case 14:
            guard i + 1 < bytes.count else { return nil }
            defer { i += 2 }
            return (UInt16(bytes[i]) << 8 | UInt16(bytes[i + 1])) &+ 269
        default: return nil
        }
    }
}

/// Random start (RFC 7252 §4.4): devices drop requests reusing a recent message id.
enum MessageIds {
    private static var next = UInt16.random(in: 0 ... .max)
    private static let lock = NSLock()

    static func take() -> UInt16 {
        lock.lock()
        defer { lock.unlock() }
        next &+= 1
        return next
    }
}
