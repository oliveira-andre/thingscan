import Foundation

/// The same shapes the Mac app saves and syncs (src-tauri/src/store.rs), so a synced device
/// decodes as-is.
enum Kind: String, Codable, CaseIterable {
    case readyToAdd = "ready_to_add"
    case hub, tv, appliance, matter
    case threadBorderRouter = "thread_border_router"
    case tracker, samsung, other

    var label: String {
        switch self {
        case .readyToAdd: "Ready to add"
        case .hub: "Hub"
        case .tv: "TV"
        case .appliance: "Appliance"
        case .matter: "Matter device"
        case .threadBorderRouter: "Thread border router"
        case .tracker: "Tracker"
        case .samsung: "Samsung device"
        case .other: "Device"
        }
    }

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Kind(rawValue: raw) ?? .other
    }
}

/// How to reach a device again; tagged by "type" like the Mac's `Endpoint`.
enum Endpoint: Codable, Hashable {
    case ocf(ip: String, di: String?)
    case upnp(location: String, udn: String?)
    case samsungTv(ip: String, id: String?, mac: String?)
    case mdns(service: String, fullname: String)
    case ble(id: String)
    case softAp(ssid: String)
    case cloud(deviceId: String)

    private enum Keys: String, CodingKey {
        case type, ip, di, location, udn, id, mac, service, fullname, ssid, deviceId
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "ocf": self = .ocf(ip: try c.decode(String.self, forKey: .ip), di: try c.decodeIfPresent(String.self, forKey: .di))
        case "upnp": self = .upnp(location: try c.decode(String.self, forKey: .location), udn: try c.decodeIfPresent(String.self, forKey: .udn))
        case "samsungTv":
            self = .samsungTv(
                ip: try c.decode(String.self, forKey: .ip),
                id: try c.decodeIfPresent(String.self, forKey: .id),
                mac: try c.decodeIfPresent(String.self, forKey: .mac)
            )
        case "mdns": self = .mdns(service: try c.decode(String.self, forKey: .service), fullname: try c.decode(String.self, forKey: .fullname))
        case "ble": self = .ble(id: try c.decode(String.self, forKey: .id))
        case "softAp": self = .softAp(ssid: try c.decode(String.self, forKey: .ssid))
        case "cloud": self = .cloud(deviceId: try c.decode(String.self, forKey: .deviceId))
        case let other: throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "unknown endpoint \(other)")
        }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: Keys.self)
        switch self {
        case let .ocf(ip, di): try c.encode("ocf", forKey: .type); try c.encode(ip, forKey: .ip); try c.encode(di, forKey: .di)
        case let .upnp(location, udn): try c.encode("upnp", forKey: .type); try c.encode(location, forKey: .location); try c.encode(udn, forKey: .udn)
        case let .samsungTv(ip, id, mac):
            try c.encode("samsungTv", forKey: .type); try c.encode(ip, forKey: .ip); try c.encode(id, forKey: .id); try c.encode(mac, forKey: .mac)
        case let .mdns(service, fullname): try c.encode("mdns", forKey: .type); try c.encode(service, forKey: .service); try c.encode(fullname, forKey: .fullname)
        case let .ble(id): try c.encode("ble", forKey: .type); try c.encode(id, forKey: .id)
        case let .softAp(ssid): try c.encode("softAp", forKey: .type); try c.encode(ssid, forKey: .ssid)
        case let .cloud(deviceId): try c.encode("cloud", forKey: .type); try c.encode(deviceId, forKey: .deviceId)
        }
    }

    var label: String {
        switch self {
        case .ocf: "OCF"
        case .upnp: "UPnP"
        case .samsungTv: "TV API"
        case .mdns: "mDNS"
        case .ble: "BLE"
        case .softAp: "Wi-Fi"
        case .cloud: "SmartThings"
        }
    }
}

struct SavedDevice: Codable, Identifiable, Hashable {
    var id: String
    var name: String
    var room: String?
    var kind: Kind
    var manufacturer: String?
    var model: String?
    var ids: [String]
    var endpoints: [Endpoint]
    var sources: [String]
    var details: [String: String]
    var addedAt: UInt64
    var lastSeen: UInt64?
    var lastAddress: String?
    var cloudId: String?
    var tvToken: String?
    /// When the user last changed it (name, room, SmartThings link), in seconds.
    /// Account sync keeps the newest edit; checks and moves don't count.
    var editedAt: UInt64
    // Phone only; never written to the account vault.
    /// Came from the Mac; removed here when the Mac removes it.
    var syncedFromMac: Bool
    /// Renamed or moved on the phone; a sync then keeps the phone's name and room.
    var editedOnPhone: Bool

    enum CodingKeys: String, CodingKey {
        case id, name, room, kind, manufacturer, model, ids, endpoints, sources, details, addedAt, lastSeen, lastAddress,
             cloudId, tvToken, editedAt, syncedFromMac, editedOnPhone
    }

    init(
        id: String = UUID().uuidString.lowercased(), name: String, room: String? = nil, kind: Kind,
        manufacturer: String? = nil, model: String? = nil, ids: [String], endpoints: [Endpoint],
        sources: [String] = [], details: [String: String] = [:], lastAddress: String? = nil
    ) {
        self.id = id
        self.name = name
        self.room = room
        self.kind = kind
        self.manufacturer = manufacturer
        self.model = model
        self.ids = ids
        self.endpoints = endpoints
        self.sources = sources
        self.details = details
        self.addedAt = UInt64(Date().timeIntervalSince1970)
        self.lastSeen = UInt64(Date().timeIntervalSince1970)
        self.lastAddress = lastAddress
        self.editedAt = addedAt
        self.syncedFromMac = false
        self.editedOnPhone = false
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        room = try c.decodeIfPresent(String.self, forKey: .room)
        kind = try c.decodeIfPresent(Kind.self, forKey: .kind) ?? .other
        manufacturer = try c.decodeIfPresent(String.self, forKey: .manufacturer)
        model = try c.decodeIfPresent(String.self, forKey: .model)
        ids = try c.decodeIfPresent([String].self, forKey: .ids) ?? []
        // Skip endpoint kinds a newer Mac might add rather than failing the whole device.
        endpoints = (try c.decodeIfPresent([Lossy<Endpoint>].self, forKey: .endpoints) ?? []).compactMap(\.value)
        sources = try c.decodeIfPresent([String].self, forKey: .sources) ?? []
        details = try c.decodeIfPresent([String: String].self, forKey: .details) ?? [:]
        addedAt = try c.decodeIfPresent(UInt64.self, forKey: .addedAt) ?? 0
        lastSeen = try c.decodeIfPresent(UInt64.self, forKey: .lastSeen)
        lastAddress = try c.decodeIfPresent(String.self, forKey: .lastAddress)
        cloudId = try c.decodeIfPresent(String.self, forKey: .cloudId)
        tvToken = try c.decodeIfPresent(String.self, forKey: .tvToken)
        // Saved before edits were timed: count adding it as the last edit, as the Mac does.
        editedAt = try c.decodeIfPresent(UInt64.self, forKey: .editedAt).flatMap { $0 > 0 ? $0 : nil } ?? addedAt
        syncedFromMac = try c.decodeIfPresent(Bool.self, forKey: .syncedFromMac) ?? false
        editedOnPhone = try c.decodeIfPresent(Bool.self, forKey: .editedOnPhone) ?? false
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(id, forKey: .id)
        try c.encode(name, forKey: .name)
        try c.encodeIfPresent(room, forKey: .room)
        try c.encode(kind, forKey: .kind)
        try c.encodeIfPresent(manufacturer, forKey: .manufacturer)
        try c.encodeIfPresent(model, forKey: .model)
        try c.encode(ids, forKey: .ids)
        try c.encode(endpoints, forKey: .endpoints)
        try c.encode(sources, forKey: .sources)
        try c.encode(details, forKey: .details)
        try c.encode(addedAt, forKey: .addedAt)
        try c.encodeIfPresent(lastSeen, forKey: .lastSeen)
        try c.encodeIfPresent(lastAddress, forKey: .lastAddress)
        try c.encodeIfPresent(cloudId, forKey: .cloudId)
        try c.encodeIfPresent(tvToken, forKey: .tvToken)
        try c.encode(editedAt, forKey: .editedAt)
        if encoder.userInfo[.forVault] as? Bool != true {
            try c.encode(syncedFromMac, forKey: .syncedFromMac)
            try c.encode(editedOnPhone, forKey: .editedOnPhone)
        }
    }

    var isTV: Bool {
        kind == .tv || endpoints.contains { if case .samsungTv = $0 { true } else { false } }
    }

    func sharesIdentity(with other: [String]) -> Bool {
        ids.contains { other.contains($0) }
    }
}

extension CodingUserInfoKey {
    /// Set when encoding for the account vault, which leaves out phone-only fields.
    static let forVault = CodingUserInfoKey(rawValue: "thingscan.forVault")!
}

/// A device removed from the home, remembered so account sync doesn't bring it back.
struct RemovedDevice: Codable, Hashable {
    var id: String
    var removedAt: UInt64
}

/// The SmartThings token (none after a disconnect) and when it was saved, renewed or removed,
/// in seconds. `oauth` is set when the token came from Thingscan's own SmartThings app and
/// renews itself; a pasted personal token has none.
struct TokenRecord: Codable, Hashable {
    var token: String?
    var savedAt: UInt64
    var oauth: OAuthGrant? = nil
}

/// What renews the access token: the SmartThings app's credentials and the current refresh
/// token. Refresh tokens work once, so every device shares this one chain through sync.
struct OAuthGrant: Codable, Hashable {
    var clientId: String
    var clientSecret: String
    var refreshToken: String
    /// When the access token stops working, in seconds.
    var expiresAt: UInt64
}

struct Lossy<T: Decodable>: Decodable {
    let value: T?
    init(from decoder: Decoder) throws { value = try? T(from: decoder) }
}

enum ReachStatus: String, Codable {
    case reachable, unreachable, unknown
}

struct Probe: Codable, Hashable {
    var via: String
    var target: String
    var status: ReachStatus
    var detail: String
    var latencyMs: Int?
}

struct Report: Codable, Hashable {
    var id: String
    var status: ReachStatus
    var address: String?
    var probes: [Probe]
}

/// A device in the SmartThings account.
struct CloudDevice: Codable, Hashable, Identifiable {
    var deviceId: String
    var name: String
    var manufacturer: String?
    var category: String?
    var ocfType: String?
    var model: String?
    var capabilities: [String]
    var di: String?
    var id: String { deviceId }
}

/// Any JSON value, for SmartThings statuses.
enum JSONValue: Codable, Hashable {
    case null, bool(Bool), number(Double), string(String), array([JSONValue]), object([String: JSONValue])

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let b = try? c.decode(Bool.self) { self = .bool(b) }
        else if let n = try? c.decode(Double.self) { self = .number(n) }
        else if let s = try? c.decode(String.self) { self = .string(s) }
        else if let a = try? c.decode([JSONValue].self) { self = .array(a) }
        else { self = .object(try c.decode([String: JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .null: try c.encodeNil()
        case let .bool(b): try c.encode(b)
        case let .number(n): try c.encode(n)
        case let .string(s): try c.encode(s)
        case let .array(a): try c.encode(a)
        case let .object(o): try c.encode(o)
        }
    }

    subscript(key: String) -> JSONValue? { if case let .object(o) = self { o[key] } else { nil } }
    var string: String? { if case let .string(s) = self { s } else { nil } }
    var double: Double? { if case let .number(n) = self { n } else { nil } }
    var array: [JSONValue]? { if case let .array(a) = self { a } else { nil } }
    var isNull: Bool { if case .null = self { true } else { false } }

    /// A readable form for "everything SmartThings reports".
    var display: String {
        switch self {
        case .null: ""
        case let .bool(b): b ? "true" : "false"
        case let .number(n): n == n.rounded() ? String(Int(n)) : String(n)
        case let .string(s): s
        case let .array(a): a.map(\.display).joined(separator: ", ")
        case .object: ""
        }
    }
}
