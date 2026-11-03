import CryptoKit
import Foundation

/// Pairing and syncing with Thingscan on the Mac (src-tauri/src/sync.rs).
///
/// Pairing proves the 6-digit code without sending it (`HMAC-SHA256(SHA-256("thingscan-code:" +
/// code), phone_public)`), then both sides derive an AES-256-GCM key from an X25519 exchange
/// with HKDF-SHA256. Sync responses are sealed with that key, so the SmartThings token and TV
/// pairings never cross the Wi-Fi in the clear.
struct MacEndpoint: Codable, Hashable, Identifiable {
    var name: String
    var host: String
    var port: Int
    var id: String { "\(host):\(port)" }
}

struct MacPairing: Codable {
    var id: String
    var macName: String
    var host: String
    var port: Int
    var pairedAt: Date
}

struct SyncPayload: Decodable {
    struct Token: Decodable {
        var token: String
        var savedAt: UInt64?
        /// Set when the Mac's token renews itself; the Mac renews it, the phone takes it.
        var oauth: OAuthGrant?
    }

    var macName: String
    var generatedAt: UInt64
    var devices: [SavedDevice]
    var smartthings: Token?
}

enum SyncError: LocalizedError {
    case message(String)
    case unreachable
    case badReply

    var errorDescription: String? {
        switch self {
        case let .message(m): m
        case .unreachable: "Couldn't reach the Mac. Is Thingscan open on it, on the same Wi-Fi?"
        case .badReply: "The Mac's reply couldn't be read. Pair again."
        }
    }
}

enum MacSync {
    static let serviceType = "_thingscan._tcp"
    private static let salt = Data("thingscan-sync-v1".utf8)
    private static let keyAccount = "mac-sync-key"
    private static let pairingDefaultsKey = "macPairing"

    /// Macs running Thingscan on this network.
    static func discover(seconds: TimeInterval = 3) async -> [MacEndpoint] {
        await Bonjour.browse(serviceType, for: seconds).compactMap { service in
            guard let port = service.txt["port"].flatMap(Int.init),
                  let ip = service.txt["ip"]?.split(separator: ",").first.map(String.init) else { return nil }
            return MacEndpoint(name: service.txt["name"] ?? service.name, host: ip, port: port)
        }
    }

    /// "192.168.3.126:47640" or "192.168.3.126" (default port), as typed by the user.
    static func endpoint(fromAddress text: String) -> MacEndpoint? {
        let parts = text.trimmingCharacters(in: .whitespaces).split(separator: ":")
        guard let host = parts.first, !host.isEmpty else { return nil }
        let port = parts.count > 1 ? Int(parts[1]) : 47640
        guard let port else { return nil }
        return MacEndpoint(name: String(host), host: String(host), port: port)
    }

    static func proof(code: String, phonePublic: Data) -> Data {
        let codeKey = SymmetricKey(data: SHA256.hash(data: Data("thingscan-code:\(code)".utf8)))
        return Data(HMAC<SHA256>.authenticationCode(for: phonePublic, using: codeKey))
    }

    static func deriveKey(shared: SharedSecret, phonePublic: Data, macPublic: Data) -> SymmetricKey {
        shared.hkdfDerivedSymmetricKey(using: SHA256.self, salt: salt, sharedInfo: phonePublic + macPublic, outputByteCount: 32)
    }

    static func pair(with mac: MacEndpoint, code: String, phoneName: String) async throws -> MacPairing {
        let privateKey = Curve25519.KeyAgreement.PrivateKey()
        let phonePublic = privateKey.publicKey.rawRepresentation
        let body: [String: String] = [
            "phoneName": phoneName,
            "publicKey": phonePublic.base64EncodedString(),
            "proof": proof(code: code, phonePublic: phonePublic).base64EncodedString(),
        ]
        let reply = try await post(url(mac.host, mac.port, "/v1/pair"), body: body)
        guard let id = reply["pairingId"] as? String,
              let macPublicText = reply["macPublicKey"] as? String,
              let macPublic = Data(base64Encoded: macPublicText),
              let macKey = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: macPublic) else { throw SyncError.badReply }
        let shared = try privateKey.sharedSecretFromKeyAgreement(with: macKey)
        let key = deriveKey(shared: shared, phonePublic: phonePublic, macPublic: macPublic)
        let pairing = MacPairing(
            id: id, macName: reply["macName"] as? String ?? mac.name, host: mac.host, port: mac.port, pairedAt: Date()
        )
        Keychain.set(key.withUnsafeBytes { Data($0) }.base64EncodedString(), for: keyAccount)
        save(pairing)
        return pairing
    }

    /// Fetches and opens the Mac's devices. Finds the Mac again over Bonjour if its address changed.
    static func sync(_ pairing: MacPairing) async throws -> (SyncPayload, MacPairing) {
        guard let key = savedKey() else { throw SyncError.message("This phone isn't paired with a Mac.") }
        var current = pairing
        let sealed: Data
        do {
            sealed = try await fetchSealed(current)
        } catch SyncError.unreachable {
            guard let moved = await discover().first(where: { $0.name == pairing.macName }) else { throw SyncError.unreachable }
            current.host = moved.host
            current.port = moved.port
            sealed = try await fetchSealed(current)
            save(current)
        }
        guard let box = try? AES.GCM.SealedBox(combined: sealed),
              let plain = try? AES.GCM.open(box, using: key, authenticating: Data(current.id.utf8)),
              let payload = try? JSONDecoder().decode(SyncPayload.self, from: plain) else { throw SyncError.badReply }
        return (payload, current)
    }

    /// Asks the Mac to send Wake-on-LAN for `mac`: the Mac can broadcast, which iOS doesn't let
    /// this app do. The request is sealed with the pairing key, so only a paired phone can ask.
    static func wake(_ pairing: MacPairing, mac: String) async throws {
        guard let key = savedKey() else { throw SyncError.message("This phone isn't paired with a Mac.") }
        let plain = try JSONSerialization.data(withJSONObject: ["mac": mac, "sentAt": Int(Date().timeIntervalSince1970)])
        guard let sealed = try AES.GCM.seal(plain, using: key, authenticating: Data(pairing.id.utf8)).combined else {
            throw SyncError.badReply
        }
        var request = URLRequest(url: url(pairing.host, pairing.port, "/v1/wake"))
        request.httpMethod = "POST"
        request.timeoutInterval = 4
        request.setValue(pairing.id, forHTTPHeaderField: "X-Thingscan-Pairing")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["sealed": sealed.base64EncodedString()])
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw SyncError.unreachable
        }
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
            throw SyncError.message(json["error"] as? String ?? "The Mac couldn't wake the TV.")
        }
    }

    private static func savedKey() -> SymmetricKey? {
        guard let text = Keychain.get(keyAccount), let data = Data(base64Encoded: text), data.count == 32 else { return nil }
        return SymmetricKey(data: data)
    }

    private static func fetchSealed(_ pairing: MacPairing) async throws -> Data {
        var request = URLRequest(url: url(pairing.host, pairing.port, "/v1/sync"))
        request.timeoutInterval = 6
        request.setValue(pairing.id, forHTTPHeaderField: "X-Thingscan-Pairing")
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw SyncError.unreachable
        }
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            throw SyncError.message(json["error"] as? String ?? "The Mac refused to sync.")
        }
        guard let sealed = (json["sealed"] as? String).flatMap({ Data(base64Encoded: $0) }) else { throw SyncError.badReply }
        return sealed
    }

    private static func post(_ url: URL, body: [String: String]) async throws -> [String: Any] {
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 8
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw SyncError.unreachable
        }
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        guard (response as? HTTPURLResponse)?.statusCode == 200 else {
            let message = json["error"] as? String ?? "The Mac refused the pairing."
            throw SyncError.message(message == "wrong code" ? "That code doesn't match the one on the Mac." : message.prefix(1).uppercased() + message.dropFirst() + ".")
        }
        return json
    }

    private static func url(_ host: String, _ port: Int, _ path: String) -> URL {
        URL(string: "http://\(host):\(port)\(path)")!
    }

    static func savedPairing() -> MacPairing? {
        UserDefaults.standard.data(forKey: pairingDefaultsKey).flatMap { try? JSONDecoder().decode(MacPairing.self, from: $0) }
    }

    static func save(_ pairing: MacPairing) {
        UserDefaults.standard.set(try? JSONEncoder().encode(pairing), forKey: pairingDefaultsKey)
    }

    static func forget() {
        UserDefaults.standard.removeObject(forKey: pairingDefaultsKey)
        Keychain.delete(keyAccount)
    }
}
