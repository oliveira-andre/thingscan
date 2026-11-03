import CommonCrypto
import CryptoKit
import Foundation

/// The Thingscan account, the iPhone side of src-tauri/src/vault.rs: the same keys, the same
/// encrypted vault and the same merge rules, so the phone and the Mac agree.
///
/// `master = PBKDF2-HMAC-SHA256(password, SHA-256("thingscan-account-v1:" + email), 600 000)`;
/// the server only ever sees `authKey = HKDF(master, "thingscan-auth")`. `HKDF(master,
/// "thingscan-wrap")` seals a random vault key, and that key seals the vault with AES-GCM.
struct AccountSession: Codable {
    var server: String
    var email: String
    var userId: String
    var token: String
    /// Base64 of the 32-byte vault key. Never sent anywhere.
    var vaultKey: String
}

/// What the vault holds, once opened.
struct VaultDoc: Codable, Equatable {
    var format: Int
    var devices: [SavedDevice]
    var removed: [RemovedDevice]
    var smartthings: TokenRecord?

    init(format: Int = ThingscanAccount.format, devices: [SavedDevice] = [], removed: [RemovedDevice] = [], smartthings: TokenRecord? = nil) {
        self.format = format
        self.devices = devices
        self.removed = removed
        self.smartthings = smartthings
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        format = try c.decodeIfPresent(Int.self, forKey: .format) ?? 0
        devices = try c.decodeIfPresent([SavedDevice].self, forKey: .devices) ?? []
        removed = try c.decodeIfPresent([RemovedDevice].self, forKey: .removed) ?? []
        smartthings = try c.decodeIfPresent(TokenRecord.self, forKey: .smartthings)
    }
}

enum AccountError: LocalizedError, Equatable {
    case message(String)
    case signedOut

    var errorDescription: String? {
        switch self {
        case let .message(m): m
        case .signedOut: "Signed out by the server. Sign in again."
        }
    }
}

enum ThingscanAccount {
    /// The vault layout this version writes. 2 added the SmartThings token's `oauth`, which an
    /// older app would drop when it wrote the vault back; those refuse a newer format instead.
    static let format = 2
    static let iterations: UInt32 = 600_000
    static let minPassword = 8
    /// Removals are forgotten after 90 days; every device will have synced by then.
    static let removedKeep: UInt64 = 90 * 24 * 3600
    private static let keychainAccount = "thingscan-account"
    private static let keyAAD = Data("thingscan-vault-key".utf8)

    struct Keys {
        let auth: Data
        let wrap: SymmetricKey
    }

    /// Stretches the password; takes a fraction of a second on purpose.
    static func deriveKeys(email: String, password: String) -> Keys {
        let salt = Array(SHA256.hash(data: Data("thingscan-account-v1:\(email)".utf8)))
        var master = [UInt8](repeating: 0, count: 32)
        _ = password.withCString { pass in
            CCKeyDerivationPBKDF(
                CCPBKDFAlgorithm(kCCPBKDF2), pass, password.utf8.count, salt, salt.count,
                CCPseudoRandomAlgorithm(kCCPRFHmacAlgSHA256), iterations, &master, master.count
            )
        }
        let ikm = SymmetricKey(data: master)
        let derive = { (info: String) in
            HKDF<SHA256>.deriveKey(inputKeyMaterial: ikm, salt: Data(), info: Data(info.utf8), outputByteCount: 32)
        }
        return Keys(auth: derive("thingscan-auth").withUnsafeBytes { Data($0) }, wrap: derive("thingscan-wrap"))
    }

    /// "sync.example.com" → "https://sync.example.com"; an explicit http:// is kept for testing on a LAN.
    static func normalizeServer(_ text: String) -> String? {
        var text = text.trimmingCharacters(in: .whitespacesAndNewlines)
        while text.hasSuffix("/") { text.removeLast() }
        let full = text.contains("://") ? text : "https://\(text)"
        guard let url = URL(string: full), ["http", "https"].contains(url.scheme ?? ""), url.host() != nil else { return nil }
        return full
    }

    // MARK: Signing in

    static func signUp(server: String, email: String, password: String, device: String) async throws -> AccountSession {
        try await begin(server: server, email: email, password: password, device: device, create: true)
    }

    static func signIn(server: String, email: String, password: String, device: String) async throws -> AccountSession {
        try await begin(server: server, email: email, password: password, device: device, create: false)
    }

    private static func begin(server: String, email: String, password: String, device: String, create: Bool) async throws -> AccountSession {
        guard let server = normalizeServer(server) else { throw AccountError.message("Enter the server's address, e.g. sync.example.com.") }
        let email = email.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard email.contains("@") else { throw AccountError.message("Enter your email address.") }
        guard !password.isEmpty else { throw AccountError.message("Enter your password.") }
        if create, password.count < minPassword {
            throw AccountError.message("Use at least \(minPassword) characters for the password.")
        }
        let keys = await Task.detached(priority: .userInitiated) { deriveKeys(email: email, password: password) }.value
        var body: [String: Any] = ["email": email, "authKey": keys.auth.base64EncodedString(), "device": device]
        let vaultKey: Data
        let reply: [String: Any]
        if create {
            vaultKey = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
            body["wrappedKey"] = try seal(vaultKey, key: keys.wrap, aad: keyAAD).base64EncodedString()
            reply = try await call("POST", server, "/v1/signup", body: body, expect: 201)
        } else {
            reply = try await call("POST", server, "/v1/login", body: body, expect: 200)
            guard let wrapped = (reply["wrappedKey"] as? String).flatMap({ Data(base64Encoded: $0) }),
                  let opened = open(wrapped, key: keys.wrap, aad: keyAAD), opened.count == 32
            else { throw AccountError.message("This account's key couldn't be opened.") }
            vaultKey = opened
        }
        guard let userId = reply["userId"] as? String, let token = reply["token"] as? String else {
            throw AccountError.message("The server's reply didn't make sense. Is that a Thingscan server?")
        }
        let session = AccountSession(
            server: server, email: reply["email"] as? String ?? email, userId: userId, token: token,
            vaultKey: vaultKey.base64EncodedString()
        )
        save(session)
        return session
    }

    /// Ends the session on the server too, if it's reachable.
    static func signOut(_ session: AccountSession) async {
        forget()
        _ = try? await call("POST", session.server, "/v1/logout", token: session.token, expect: 204)
    }

    // MARK: The vault

    /// The vault and its version; `doc` is nil when it's still the `known` version.
    static func fetch(_ session: AccountSession, known: Int?) async throws -> (version: Int, doc: VaultDoc?) {
        let path = known.map { "/v1/vault?known=\($0)" } ?? "/v1/vault"
        let reply = try await call("GET", session.server, path, token: session.token, expect: 200)
        let version = reply["version"] as? Int ?? 0
        if reply["unchanged"] as? Bool == true { return (version, nil) }
        guard let text = reply["vault"] as? String else { return (version, VaultDoc(format: 0)) }
        guard let sealed = Data(base64Encoded: text), let key = vaultKey(session),
              let plain = open(sealed, key: key, aad: Data("thingscan-vault:\(session.userId)".utf8))
        else { throw AccountError.message("Your account's data couldn't be opened. Sign out and in again.") }
        let doc: VaultDoc
        do {
            doc = try JSONDecoder().decode(VaultDoc.self, from: plain)
        } catch {
            throw AccountError.message("Your account's data couldn't be read. Update Thingscan.")
        }
        if doc.format > format { throw AccountError.message("A newer Thingscan saved your account's data. Update Thingscan on this iPhone.") }
        return (version, doc)
    }

    /// Writes the vault based on `baseVersion`; nil when another device wrote first.
    static func push(_ session: AccountSession, _ doc: VaultDoc, baseVersion: Int) async throws -> Int? {
        let encoder = JSONEncoder()
        encoder.userInfo[.forVault] = true
        guard let key = vaultKey(session) else { throw AccountError.message("The saved vault key is damaged. Sign in again.") }
        let sealed = try seal(try encoder.encode(doc), key: key, aad: Data("thingscan-vault:\(session.userId)".utf8))
        do {
            let reply = try await call(
                "PUT", session.server, "/v1/vault", token: session.token,
                body: ["baseVersion": baseVersion, "vault": sealed.base64EncodedString()], expect: 200
            )
            return reply["version"] as? Int ?? baseVersion + 1
        } catch HTTPStatus.conflict {
            return nil
        }
    }

    private static func vaultKey(_ session: AccountSession) -> SymmetricKey? {
        guard let data = Data(base64Encoded: session.vaultKey), data.count == 32 else { return nil }
        return SymmetricKey(data: data)
    }

    // MARK: Merging (the same rules as `merge` in src-tauri/src/vault.rs)

    /// Combines this phone's state with the vault's; `remote` wins ties so devices merging the
    /// same inputs agree. Name, room and SmartThings link come from the newest edit;
    /// addresses and endpoints from whoever saw the device last; a removal beats older edits;
    /// the newest SmartThings token (or its removal) wins.
    static func merge(local: VaultDoc, remote: VaultDoc, now: UInt64 = UInt64(Date().timeIntervalSince1970)) -> VaultDoc {
        var removed: [String: UInt64] = [:]
        for r in local.removed + remote.removed { removed[r.id] = max(removed[r.id] ?? 0, r.removedAt) }
        var ids = remote.devices.map(\.id)
        ids += local.devices.map(\.id).filter { id in !remote.devices.contains { $0.id == id } }

        var devices: [SavedDevice] = []
        for id in ids {
            let mine = local.devices.first { $0.id == id }
            let theirs = remote.devices.first { $0.id == id }
            let device: SavedDevice
            switch (mine, theirs) {
            case let (mine?, theirs?): device = mergeDevice(mine, theirs)
            case let (only?, nil), let (nil, only?): device = only
            case (nil, nil): continue
            }
            if let at = removed[id] {
                if at >= device.editedAt { continue }
                removed[id] = nil // edited again after it was removed somewhere: keep it
            }
            devices.append(device)
        }
        let cutoff = now > removedKeep ? now - removedKeep : 0
        let kept = removed.filter { $0.value >= cutoff }.map { RemovedDevice(id: $0.key, removedAt: $0.value) }
        let smartthings: TokenRecord? = switch (local.smartthings, remote.smartthings) {
        case let (mine?, theirs?): mine.savedAt > theirs.savedAt ? mine : theirs
        case let (mine, theirs): theirs ?? mine
        }
        return VaultDoc(devices: devices, removed: kept.sorted { $0.id.utf8.lexicographicallyPrecedes($1.id.utf8) }, smartthings: smartthings)
    }

    static func mergeDevice(_ mine: SavedDevice, _ theirs: SavedDevice) -> SavedDevice {
        let edits = mine.editedAt > theirs.editedAt ? mine : theirs
        let (seen, other) = (mine.lastSeen ?? 0) > (theirs.lastSeen ?? 0) ? (mine, theirs) : (theirs, mine)
        var out = seen
        out.name = edits.name
        out.room = edits.room
        out.manufacturer = seen.manufacturer ?? other.manufacturer
        out.model = seen.model ?? other.model
        for id in other.ids where !out.ids.contains(id) { out.ids.append(id) }
        for source in other.sources where !out.sources.contains(source) { out.sources.append(source) }
        for (key, value) in other.details where out.details[key] == nil { out.details[key] = value }
        // The SmartThings link follows the newest edit; its id and endpoint go last.
        out.ids.removeAll { $0.hasPrefix("st:") }
        out.endpoints.removeAll { if case .cloud = $0 { true } else { false } }
        if let cloudId = edits.cloudId {
            out.ids.append("st:\(cloudId)")
            out.endpoints.append(.cloud(deviceId: cloudId))
        }
        out.cloudId = edits.cloudId
        out.lastSeen = [mine.lastSeen, theirs.lastSeen].compactMap { $0 }.max()
        out.addedAt = [mine.addedAt, theirs.addedAt].filter { $0 > 0 }.min() ?? 0
        out.editedAt = max(mine.editedAt, theirs.editedAt)
        return out
    }

    /// Equal apart from when devices were last seen (every check changes that) and what only
    /// the phone keeps, so neither is worth a write.
    static func sameForVault(_ a: VaultDoc, _ b: VaultDoc) -> Bool {
        func strip(_ doc: VaultDoc) -> VaultDoc {
            var doc = doc
            for i in doc.devices.indices {
                doc.devices[i].lastSeen = nil
                doc.devices[i].syncedFromMac = false
                doc.devices[i].editedOnPhone = false
                doc.devices[i].sources.sort()
            }
            doc.removed.sort { $0.id < $1.id }
            return doc
        }
        return strip(a) == strip(b)
    }

    // MARK: Storage and plumbing

    static func savedSession() -> AccountSession? {
        Keychain.get(keychainAccount).flatMap { try? JSONDecoder().decode(AccountSession.self, from: Data($0.utf8)) }
    }

    static func save(_ session: AccountSession) {
        if let data = try? JSONEncoder().encode(session) { Keychain.set(String(decoding: data, as: UTF8.self), for: keychainAccount) }
    }

    static func forget() {
        Keychain.delete(keychainAccount)
    }

    private static func seal(_ data: Data, key: SymmetricKey, aad: Data) throws -> Data {
        guard let combined = try AES.GCM.seal(data, using: key, authenticating: aad).combined else {
            throw AccountError.message("Couldn't encrypt.")
        }
        return combined
    }

    static func open(_ sealed: Data, key: SymmetricKey, aad: Data) -> Data? {
        guard let box = try? AES.GCM.SealedBox(combined: sealed) else { return nil }
        return try? AES.GCM.open(box, using: key, authenticating: aad)
    }

    private enum HTTPStatus: Error { case conflict }

    private static func call(
        _ method: String, _ server: String, _ path: String, token: String? = nil, body: [String: Any]? = nil, expect: Int
    ) async throws -> [String: Any] {
        guard let url = URL(string: server + path) else { throw AccountError.message("Enter the server's address, e.g. sync.example.com.") }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 20
        if let token { request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization") }
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response): (Data, URLResponse)
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw AccountError.message("Couldn't reach \(url.host() ?? server).")
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:]
        if status == expect { return json }
        if status == 409, path.hasPrefix("/v1/vault") { throw HTTPStatus.conflict }
        if status == 401, token != nil { throw AccountError.signedOut }
        if let message = json["error"] as? String { throw AccountError.message(message) }
        throw AccountError.message(status == 404 ? "That address isn't a Thingscan server." : "The server answered \(status).")
    }
}
