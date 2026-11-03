import Foundation

/// A small CBOR (RFC 8949) decoder for OCF payloads, including the indefinite-length maps
/// Samsung TVs send.
indirect enum CBOR {
    case unsigned(UInt64), negative(Int64), bytes([UInt8]), text(String), array([CBOR]), map([(CBOR, CBOR)])
    case tagged(UInt64, CBOR), bool(Bool), null, float(Double)

    static func decode(_ bytes: [UInt8]) -> CBOR? {
        var reader = Reader(bytes: bytes)
        return try? reader.item()
    }

    subscript(key: String) -> CBOR? {
        guard case let .map(pairs) = self else { return nil }
        return pairs.first { if case let .text(k) = $0.0 { k == key } else { false } }?.1
    }

    var text: String? { if case let .text(s) = self { s } else { nil } }
    var array: [CBOR]? { if case let .array(a) = self { a } else { nil } }

    /// A text value or an array of texts (OCF `rt` can be either).
    var texts: [String] {
        switch self {
        case let .text(s): [s]
        case let .array(items): items.compactMap(\.text)
        default: []
        }
    }

    private struct Reader {
        let bytes: [UInt8]
        var i = 0

        enum Failure: Error { case truncated, unsupported }

        mutating func byte() throws -> UInt8 {
            guard i < bytes.count else { throw Failure.truncated }
            defer { i += 1 }
            return bytes[i]
        }

        mutating func take(_ count: Int) throws -> [UInt8] {
            guard count >= 0, i + count <= bytes.count else { throw Failure.truncated }
            defer { i += count }
            return Array(bytes[i ..< i + count])
        }

        mutating func argument(_ info: UInt8) throws -> UInt64? {
            switch info {
            case 0 ... 23: return UInt64(info)
            case 24: return UInt64(try byte())
            case 25: return try take(2).reduce(0) { $0 << 8 | UInt64($1) }
            case 26: return try take(4).reduce(0) { $0 << 8 | UInt64($1) }
            case 27: return try take(8).reduce(0) { $0 << 8 | UInt64($1) }
            case 31: return nil // indefinite length
            default: throw Failure.unsupported
            }
        }

        mutating func isBreak() -> Bool {
            if i < bytes.count, bytes[i] == 0xFF { i += 1; return true }
            return false
        }

        mutating func item() throws -> CBOR {
            let initial = try byte()
            let major = initial >> 5
            let info = initial & 0x1F
            switch major {
            case 0: return .unsigned(try argument(info) ?? 0)
            case 1: return .negative(-1 - Int64(try argument(info) ?? 0))
            case 2, 3:
                var raw: [UInt8] = []
                if let length = try argument(info) {
                    raw = try take(Int(length))
                } else {
                    while !isBreak() {
                        let chunk = try item()
                        if case let .bytes(b) = chunk { raw += b } else if case let .text(t) = chunk { raw += Array(t.utf8) }
                    }
                }
                return major == 2 ? .bytes(raw) : .text(String(decoding: raw, as: UTF8.self))
            case 4:
                var items: [CBOR] = []
                if let count = try argument(info) {
                    for _ in 0 ..< count { items.append(try item()) }
                } else {
                    while !isBreak() { items.append(try item()) }
                }
                return .array(items)
            case 5:
                var pairs: [(CBOR, CBOR)] = []
                if let count = try argument(info) {
                    for _ in 0 ..< count { pairs.append((try item(), try item())) }
                } else {
                    while !isBreak() { pairs.append((try item(), try item())) }
                }
                return .map(pairs)
            case 6: return .tagged(try argument(info) ?? 0, try item())
            default:
                switch info {
                case 20: return .bool(false)
                case 21: return .bool(true)
                case 22, 23: return .null
                case 25: return .float(Double(halfToFloat(UInt16(try take(2).reduce(0) { $0 << 8 | UInt64($1) }))))
                case 26: return .float(Double(Float(bitPattern: UInt32(try take(4).reduce(0) { $0 << 8 | UInt64($1) }))))
                case 27: return .float(Double(bitPattern: try take(8).reduce(0) { $0 << 8 | UInt64($1) }))
                default: return .null
                }
            }
        }

        func halfToFloat(_ h: UInt16) -> Float {
            let sign: Float = h & 0x8000 != 0 ? -1 : 1
            let exponent = Int((h >> 10) & 0x1F)
            let mantissa = Float(h & 0x3FF)
            if exponent == 0 { return sign * mantissa * pow(2, -24) }
            if exponent == 31 { return mantissa == 0 ? sign * .infinity : .nan }
            return sign * (1 + mantissa / 1024) * pow(2, Float(exponent - 15))
        }
    }
}
