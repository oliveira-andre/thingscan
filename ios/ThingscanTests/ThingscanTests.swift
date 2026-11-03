import CryptoKit
import XCTest
@testable import Thingscan

final class SyncCryptoTests: XCTestCase {
    private func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    /// The same fixed inputs as `cross_platform_vectors` in src-tauri/src/sync.rs.
    func testMatchesTheMacSide() throws {
        let phone = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 1, count: 32))
        let mac = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: Data(repeating: 2, count: 32))
        let phonePublic = phone.publicKey.rawRepresentation
        let macPublic = mac.publicKey.rawRepresentation
        XCTAssertEqual(hex(phonePublic), "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209")
        XCTAssertEqual(hex(macPublic), "ce8d3ad1ccb633ec7b70c17814a5c76ecd029685050d344745ba05870e587d59")

        let key = MacSync.deriveKey(shared: try phone.sharedSecretFromKeyAgreement(with: mac.publicKey), phonePublic: phonePublic, macPublic: macPublic)
        XCTAssertEqual(hex(key.withUnsafeBytes { Data($0) }), "081bfdcb2e599ce2ec36c70f1747ea1f435f17b8396d38ed0307868121af08b9")
        XCTAssertEqual(hex(MacSync.proof(code: "123456", phonePublic: phonePublic)), "de2dfa6e68890b43b6a70d5ffdeec491dc7b9e8d0fd294d4e0b761c01d1a34e5")
    }

    func testReadsTypedAddresses() {
        XCTAssertEqual(MacSync.endpoint(fromAddress: " 192.168.3.126:47641 ")?.id, "192.168.3.126:47641")
        XCTAssertEqual(MacSync.endpoint(fromAddress: "192.168.3.126")?.port, 47640)
        XCTAssertNil(MacSync.endpoint(fromAddress: "192.168.3.126:port"))
        XCTAssertNil(MacSync.endpoint(fromAddress: ""))
    }
}

final class CodecTests: XCTestCase {
    func testCoAPRoundTripsWithBlockOption() throws {
        let request = CoAPMessage.get("/oic/res", messageId: 0x1234, token: [1, 2]).forBlock(messageId: 0x1235, num: 3, szx: 6)
        let decoded = try XCTUnwrap(CoAPMessage.decode(request.encode()))
        XCTAssertEqual(decoded.messageId, 0x1235)
        XCTAssertEqual(decoded.token, [1, 2])
        XCTAssertEqual(decoded.options.filter { $0.0 == CoAPMessage.uriPath }.map { String(decoding: $0.1, as: UTF8.self) }, ["oic", "res"])
        XCTAssertEqual(decoded.block?.num, 3)
        XCTAssertEqual(decoded.block?.szx, 6)
    }

    /// Samsung TVs answer /oic/d with indefinite-length maps.
    func testDecodesIndefiniteMaps() throws {
        // {_ "n": "TV", "rt": ["oic.wk.d"]}
        let bytes: [UInt8] = [0xBF, 0x61, 0x6E, 0x62, 0x54, 0x56, 0x62, 0x72, 0x74, 0x81, 0x68] + Array("oic.wk.d".utf8) + [0xFF]
        let value = try XCTUnwrap(CBOR.decode(bytes))
        XCTAssertEqual(value["n"]?.text, "TV")
        XCTAssertEqual(value["rt"]?.texts, ["oic.wk.d"])
    }
}

final class AccountTests: XCTestCase {
    private func hex(_ data: Data) -> String { data.map { String(format: "%02x", $0) }.joined() }

    /// The same inputs as `account_key_vectors` in src-tauri/src/vault.rs.
    func testDerivesTheSameKeysAsTheMac() {
        let keys = ThingscanAccount.deriveKeys(email: "andre@example.com", password: "correct horse battery")
        XCTAssertEqual(hex(keys.auth), "c5dcfc412dd47a78992646bd529d1ce027d35993527788ff835f5b2bd949c757")
        XCTAssertEqual(hex(keys.wrap.withUnsafeBytes { Data($0) }), "0d4af68e9a2161e172273a393342e1ea67de3607470a918611d23526a0e3289f")
    }

    /// A vault the Mac sealed (same test) opens and decodes here.
    func testOpensAVaultTheMacSealed() throws {
        let sealed = try XCTUnwrap(Data(base64Encoded: "gAbei4flM44zC38IqgRFz3AG5+jg+Mn5Q9+006gVVYOjtAWS48dgAGNbOBB+B2psMMcmYwynR9+t2AeF/U81eMgUMWAxrz0GQs0WrKLzZq8gfBH2veqoK7s3wZYfIEwJv2a1/W48d44WJAuLv4S7Mb5UmZqiZwPPv0Z8c56aUztJkdQday8eaWH+pQ5AXxMkgxrN/ojxLuy8SyyCYdYgjAEOgvp6Cm38syl9Vl1JutaDQCwW+WTVuLCNcy6Aa55IOCpwFqT9TZT0erwahGjqW+qs2y3L34UUk0DqRiiQzuEF55erUZwLAZw29wAz1eTYbNwcIi6jiRxAu6/qKXRg5dMliTbN6QXZPYrmJG3DAOkRijDvekeWfT8ZU7W50PoIh3SX5VSyyzH5XYWBIl1kzanaL2DWUPcGdUA8Ce93Z4ST"))
        let plain = try XCTUnwrap(ThingscanAccount.open(sealed, key: SymmetricKey(data: Data(repeating: 5, count: 32)), aad: Data("thingscan-vault:u1".utf8)))
        let doc = try JSONDecoder().decode(VaultDoc.self, from: plain)
        XCTAssertEqual(doc.format, 1)
        XCTAssertEqual(doc.devices.map(\.name), ["TV"])
        XCTAssertEqual(doc.devices[0].editedAt, 1)
        XCTAssertEqual(doc.devices[0].lastSeen, 10)
        XCTAssertEqual(doc.devices[0].lastAddress, "10.0.0.10")
        XCTAssertNil(ThingscanAccount.open(sealed, key: SymmetricKey(data: Data(repeating: 5, count: 32)), aad: Data("thingscan-vault:u2".utf8)))
    }

    /// What goes into the vault leaves out what only the phone keeps.
    func testVaultEncodingLeavesOutPhoneOnlyFields() throws {
        let encoder = JSONEncoder()
        encoder.userInfo[.forVault] = true
        let json = String(decoding: try encoder.encode(device("tv", "TV", edited: 1, seen: 1)), as: UTF8.self)
        XCTAssertFalse(json.contains("syncedFromMac"))
        XCTAssertTrue(json.contains("\"editedAt\":1"))
    }

    private func device(_ id: String, _ name: String, edited: UInt64, seen: UInt64?) -> SavedDevice {
        var d = SavedDevice(id: id, name: name, kind: .tv, ids: ["upnp:\(id)"], endpoints: [])
        d.addedAt = 100
        d.lastSeen = seen
        d.lastAddress = seen.map { "10.0.0.\($0 % 250)" }
        d.editedAt = edited
        return d
    }

    // The same cases as the merge tests in src-tauri/src/vault.rs.

    func testNewestEditWinsAndTheNewestSightingSuppliesTheFacts() {
        var mine = device("tv", "TV", edited: 200, seen: 500)
        mine.room = "Den"
        var theirs = device("tv", "Living room TV", edited: 300, seen: 400)
        theirs.ids.append("ocf:abc")
        let merged = ThingscanAccount.merge(local: VaultDoc(devices: [mine]), remote: VaultDoc(devices: [theirs]))
        let tv = merged.devices[0]
        XCTAssertEqual(tv.name, "Living room TV")
        XCTAssertNil(tv.room)
        XCTAssertEqual(tv.lastAddress, "10.0.0.0")
        XCTAssertEqual(tv.ids, ["upnp:tv", "ocf:abc"])
        XCTAssertEqual(tv.lastSeen, 500)
        XCTAssertEqual(tv.editedAt, 300)
        XCTAssertEqual(ThingscanAccount.merge(local: merged, remote: merged), merged)
    }

    func testRemovalsBeatOlderEditsButNotNewerOnes() {
        let now: UInt64 = 1_800_000_000
        let kept = device("a", "A", edited: now, seen: nil)
        let gone = device("b", "B", edited: now - 50, seen: nil)
        let local = VaultDoc(removed: [RemovedDevice(id: "a", removedAt: now - 10), RemovedDevice(id: "b", removedAt: now - 10)])
        let merged = ThingscanAccount.merge(local: local, remote: VaultDoc(devices: [kept, gone]), now: now)
        XCTAssertEqual(merged.devices, [kept])
        XCTAssertEqual(merged.removed, [RemovedDevice(id: "b", removedAt: now - 10)])
        let old = VaultDoc(removed: [RemovedDevice(id: "z", removedAt: 5)])
        XCTAssertTrue(ThingscanAccount.merge(local: old, remote: VaultDoc(), now: now).removed.isEmpty)
    }

    func testTheSmartThingsLinkAndTokenFollowTheNewestChange() {
        var linked = device("ac", "A/C", edited: 300, seen: nil)
        linked.cloudId = "c1"
        linked.ids.append("st:c1")
        linked.endpoints.append(.cloud(deviceId: "c1"))
        let local = VaultDoc(devices: [device("ac", "A/C", edited: 200, seen: nil)], smartthings: TokenRecord(token: nil, savedAt: 50))
        let remote = VaultDoc(devices: [linked], smartthings: TokenRecord(token: "t", savedAt: 40))
        let merged = ThingscanAccount.merge(local: local, remote: remote)
        XCTAssertEqual(merged.devices[0].cloudId, "c1")
        XCTAssertEqual(merged.devices[0].ids, linked.ids)
        XCTAssertEqual(merged.devices[0].endpoints, linked.endpoints)
        XCTAssertEqual(merged.smartthings, TokenRecord(token: nil, savedAt: 50))
    }

    func testLastSeenAloneIsntWorthAWrite() {
        let a = VaultDoc(devices: [device("tv", "TV", edited: 1, seen: 10)])
        var b = a
        b.devices[0].lastSeen = 99
        b.devices[0].editedOnPhone = true
        XCTAssertTrue(ThingscanAccount.sameForVault(a, b))
        b.devices[0].name = "TV 2"
        XCTAssertFalse(ThingscanAccount.sameForVault(a, b))
    }

    func testReadsServerAddresses() {
        XCTAssertEqual(ThingscanAccount.normalizeServer(" sync.example.com/ "), "https://sync.example.com")
        XCTAssertEqual(ThingscanAccount.normalizeServer("http://192.168.3.10:8080"), "http://192.168.3.10:8080")
        XCTAssertNil(ThingscanAccount.normalizeServer(""))
        XCTAssertNil(ThingscanAccount.normalizeServer("ftp://x"))
    }
}

final class SmartThingsOAuthTests: XCTestCase {
    private let grant = OAuthGrant(clientId: "cid", clientSecret: "secret", refreshToken: "rt-1", expiresAt: 1_800_086_400)

    /// The same shape the Mac writes (src-tauri/src/account.rs): `oauth` only when it renews itself.
    func testTokenRecordsRoundTripWithAndWithoutOAuth() throws {
        let renewing = TokenRecord(token: "at-1", savedAt: 1_800_000_000, oauth: grant)
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(renewing)) as? [String: Any]
        let oauth = try XCTUnwrap(json?["oauth"] as? [String: Any])
        XCTAssertEqual(oauth["clientId"] as? String, "cid")
        XCTAssertEqual(oauth["refreshToken"] as? String, "rt-1")
        XCTAssertEqual(oauth["expiresAt"] as? Int, 1_800_086_400)
        XCTAssertEqual(try JSONDecoder().decode(TokenRecord.self, from: JSONEncoder().encode(renewing)), renewing)

        let pasted = try JSONDecoder().decode(TokenRecord.self, from: Data(#"{"token":"pat","savedAt":5}"#.utf8))
        XCTAssertEqual(pasted, TokenRecord(token: "pat", savedAt: 5))
        XCTAssertFalse(String(decoding: try JSONEncoder().encode(pasted), as: UTF8.self).contains("oauth"))
    }

    func testTheNewestRenewalWinsTheMergeWithItsGrant() {
        var renewed = grant
        renewed.refreshToken = "rt-2"
        let local = VaultDoc(smartthings: TokenRecord(token: "at-1", savedAt: 100, oauth: grant))
        let remote = VaultDoc(smartthings: TokenRecord(token: "at-2", savedAt: 200, oauth: renewed))
        XCTAssertEqual(ThingscanAccount.merge(local: local, remote: remote).smartthings?.oauth?.refreshToken, "rt-2")
        XCTAssertEqual(ThingscanAccount.merge(local: remote, remote: local).smartthings?.oauth?.refreshToken, "rt-2")
        XCTAssertEqual(ThingscanAccount.format, 2)
    }

    func testTheMacSendsItsGrantAlong() throws {
        let json = #"{"macName":"Mac","generatedAt":1,"devices":[],"smartthings":{"token":"at","savedAt":9,"oauth":{"clientId":"cid","clientSecret":"secret","refreshToken":"rt-1","expiresAt":1800086400}}}"#
        let payload = try JSONDecoder().decode(SyncPayload.self, from: Data(json.utf8))
        XCTAssertEqual(payload.smartthings?.oauth, grant)
    }

    func testBuildsTheSignInAddress() {
        let ours = SmartThingsOAuth.authorizeURL(app: .init(clientId: "c-1", clientSecret: "s", ours: true), state: "abc")
        XCTAssertEqual(
            ours.absoluteString,
            "https://api.smartthings.com/oauth/authorize?client_id=c-1&response_type=code&redirect_uri=https%3A%2F%2Fhttpbin.org%2Fget&state=abc&scope=r%3Adevices%3A*%20w%3Adevices%3A*%20x%3Adevices%3A*%20r%3Alocations%3A*"
        )
        // An app the user brought keeps its own scopes: SmartThings offers them.
        let theirs = SmartThingsOAuth.authorizeURL(app: .init(clientId: "c-1", clientSecret: "s", ours: false), state: "abc")
        XCTAssertFalse(theirs.absoluteString.contains("scope="))
    }

    func testReadsTheSignInRedirect() {
        let url = { (s: String) in URL(string: s)! }
        XCTAssertEqual(try SmartThingsOAuth.callback(url("https://httpbin.org/get?code=Ab12Cd&state=s1"), state: "s1")?.get(), "Ab12Cd")
        XCTAssertThrowsError(try SmartThingsOAuth.callback(url("https://httpbin.org/get?code=Ab12Cd&state=other"), state: "s1")?.get())
        XCTAssertThrowsError(try SmartThingsOAuth.callback(url("https://httpbin.org/get?error=access_denied&state=s1"), state: "s1")?.get())
        XCTAssertNil(SmartThingsOAuth.callback(url("https://account.samsung.com/login?code=x&state=s1"), state: "s1"))
    }

    func testReadsTokenReplies() throws {
        let reply = try JSONDecoder().decode(JSONValue.self, from: Data(#"{"token_type":"bearer","access_token":"at","refresh_token":"rt","expires_in":86399,"scope":"r:devices:*"}"#.utf8))
        let tokens = try XCTUnwrap(SmartThingsOAuth.parseTokens(reply))
        XCTAssertEqual(tokens, SmartThingsOAuth.Tokens(accessToken: "at", refreshToken: "rt", expiresIn: 86399))
        let kept = tokens.grant(for: .init(clientId: "cid", clientSecret: "secret", ours: true), now: 1000)
        XCTAssertEqual(kept, OAuthGrant(clientId: "cid", clientSecret: "secret", refreshToken: "rt", expiresAt: 87399))
        XCTAssertNil(SmartThingsOAuth.parseTokens(.object(["error": .string("invalid_grant")])))
    }

    /// The same packet as `builds_wake_on_lan_packets` in crates/thingscan-core/src/tv.rs.
    func testBuildsWakeOnLANPackets() throws {
        let packet = try XCTUnwrap(WakeOnLAN.packet(mac: "04:E4:B6:B3:90:82"))
        XCTAssertEqual(packet.count, 102)
        XCTAssertEqual(Array(packet.prefix(6)), [UInt8](repeating: 0xFF, count: 6))
        XCTAssertEqual(Array(packet.suffix(6)), [0x04, 0xE4, 0xB6, 0xB3, 0x90, 0x82])
        XCTAssertEqual(WakeOnLAN.packet(mac: "04-e4-b6-b3-90-82"), packet)
        XCTAssertNil(WakeOnLAN.packet(mac: "04:E4:B6"))
        XCTAssertNil(WakeOnLAN.packet(mac: "zz:E4:B6:B3:90:82"))
    }
}
