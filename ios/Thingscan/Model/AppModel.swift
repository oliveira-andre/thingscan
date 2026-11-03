import Foundation
import Observation
import UIKit

enum CheckState: Equatable {
    case checking
    case done(Report)
}

/// The phone's home: saved devices and their reachability, the SmartThings account, the
/// Thingscan account that syncs it all, and the direct pairing with Thingscan on the Mac.
@MainActor
@Observable
final class AppModel {
    private(set) var devices: [SavedDevice] = []
    private(set) var checks: [String: CheckState] = [:]
    private(set) var checking = false
    private(set) var lastCheck: Date?

    // SmartThings
    private(set) var hasCloudToken = false
    private(set) var tokenHint: String?
    private(set) var tokenSavedAt: Date?
    /// The token came from Thingscan's own SmartThings app and renews itself.
    private(set) var cloudRenews = false
    private(set) var cloudDevices: [CloudDevice] = []
    private(set) var cloudError: String?
    private var renewing: Task<Void, Never>?
    private var recovering: Task<String?, Never>?
    /// The token was signed in for on this iPhone (or renewed from one that was). The phone
    /// renews it itself and keeps it over the paired Mac's, which has a chain of its own.
    private var ownGrant = false

    // Mac
    private(set) var pairing: MacPairing?
    private(set) var lastSync: Date?
    private(set) var syncing = false
    private(set) var syncError: String?

    // Thingscan account
    private(set) var accountSession: AccountSession?
    private(set) var accountSyncing = false
    private(set) var accountError: String?
    private(set) var lastAccountSync: Date?
    /// Devices removed here, remembered so account sync doesn't bring them back.
    private var removed: [RemovedDevice] = []
    /// The last vault read or written, so an unchanged one needn't be downloaded again.
    private var remoteVault: (version: Int, doc: VaultDoc)?
    private var syncAgain = false
    private var pendingSync: Task<Void, Never>?
    private var poller: Task<Void, Never>?

    private static let tokenAccount = "smartthings-token"
    private static let grantAccount = "smartthings-oauth"
    /// The SmartThings app Thingscan made (or the user brought), kept so connecting again
    /// doesn't make another: its secret can't be read back.
    private static let appAccount = "smartthings-app"
    /// Renew when the access token has less than this left.
    private static let renewAhead: UInt64 = 6 * 3600
    private let file: URL = {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir.appendingPathComponent("devices.json")
    }()
    private let removedFile: URL = {
        let dir = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return dir.appendingPathComponent("removed.json")
    }()

    init() {
        load()
        accountSession = ThingscanAccount.savedSession()
        lastAccountSync = UserDefaults.standard.object(forKey: "lastAccountSync") as? Date
        pairing = MacSync.savedPairing()
        lastSync = UserDefaults.standard.object(forKey: "lastSync") as? Date
        tokenSavedAt = UserDefaults.standard.object(forKey: "tokenSavedAt") as? Date
        ownGrant = UserDefaults.standard.bool(forKey: "ownGrant")
        if let token = Keychain.get(Self.tokenAccount) {
            hasCloudToken = true
            tokenHint = String(token.suffix(4))
            cloudRenews = grant != nil
        }
    }

    /// The SmartThings API. With a token that renews itself, a refused request is retried once
    /// with a newer token: synced from another device, or renewed here.
    var cloud: SmartThingsClient? {
        guard let token = Keychain.get(Self.tokenAccount) else { return nil }
        guard cloudRenews else { return SmartThingsClient(token: token) }
        return SmartThingsClient(token: token, renew: { [weak self] failed in await self?.freshToken(after: failed) })
    }

    private var grant: OAuthGrant? {
        Keychain.get(Self.grantAccount).flatMap { try? JSONDecoder().decode(OAuthGrant.self, from: Data($0.utf8)) }
    }

    var rooms: [String] { Array(Set(devices.compactMap(\.room))).sorted() }

    func device(_ id: String) -> SavedDevice? { devices.first { $0.id == id } }

    // MARK: Opening the app

    /// Sync the Thingscan account (it may carry a newer token), refresh the SmartThings account
    /// and check every device. The paired Mac is synced last: it may be closed, asleep or far
    /// away, and the phone's own work never waits on it.
    func refreshAll() async {
        if accountSession != nil { await syncAccount() }
        await renewCloudIfNeeded()
        if hasCloudToken { await refreshCloud() }
        await checkAll()
        guard accountSession == nil, pairing != nil else { return }
        let token = Keychain.get(Self.tokenAccount)
        await syncNow()
        if hasCloudToken, Keychain.get(Self.tokenAccount) != token { await refreshCloud() }
        let arrived = devices.filter { checks[$0.id] == nil }.map(\.id)
        if !arrived.isEmpty { await checkAll(ids: arrived) }
    }

    // MARK: Devices

    func add(_ scanned: ScannedDevice, name: String, room: String?) -> SavedDevice {
        if let index = devices.firstIndex(where: { $0.sharesIdentity(with: scanned.ids) }) {
            devices[index].name = clean(name) ?? devices[index].name
            devices[index].room = clean(room)
            devices[index].endpoints = scanned.endpoints
            devices[index].editedOnPhone = true
            devices[index].editedAt = Self.now
            edited()
            return devices[index]
        }
        var device = SavedDevice(
            name: clean(name) ?? scanned.name, room: clean(room), kind: scanned.kind, manufacturer: scanned.manufacturer,
            model: scanned.model, ids: scanned.ids, endpoints: scanned.endpoints, sources: scanned.sources,
            details: scanned.details, lastAddress: scanned.address
        )
        if let match = cloudDevices.first(where: { cloudIds($0).contains { device.ids.contains($0) } }) {
            attach(&device, match)
        }
        devices.append(device)
        edited()
        return device
    }

    func addCloud(_ cloud: CloudDevice, name: String, room: String?) -> SavedDevice {
        if let index = devices.firstIndex(where: { $0.sharesIdentity(with: cloudIds(cloud)) }) {
            devices[index].name = clean(name) ?? devices[index].name
            devices[index].room = clean(room) ?? devices[index].room
            attach(&devices[index], cloud)
            devices[index].editedAt = Self.now
            edited()
            return devices[index]
        }
        var device = SavedDevice(
            name: clean(name) ?? cloud.name, room: clean(room), kind: Self.kind(ofCategory: cloud.category),
            manufacturer: cloud.manufacturer, model: cloud.model, ids: [], endpoints: []
        )
        device.lastSeen = nil
        if let category = cloud.category { device.details["cloud.category"] = category }
        attach(&device, cloud)
        devices.append(device)
        edited()
        return device
    }

    func update(_ id: String, name: String, room: String?) {
        guard let index = devices.firstIndex(where: { $0.id == id }) else { return }
        devices[index].name = clean(name) ?? devices[index].name
        devices[index].room = clean(room)
        devices[index].editedOnPhone = true
        devices[index].editedAt = Self.now
        edited()
    }

    func renameRoom(_ from: String, to: String?) {
        for index in devices.indices where devices[index].room == from {
            devices[index].room = clean(to)
            devices[index].editedOnPhone = true
            devices[index].editedAt = Self.now
        }
        edited()
    }

    func remove(_ id: String) {
        devices.removeAll { $0.id == id }
        checks[id] = nil
        removed.append(RemovedDevice(id: id, removedAt: Self.now))
        edited()
    }

    /// Keeps the MAC address the TV reported while on, so Wake-on-LAN can turn it on later. Like
    /// the Mac, it goes on the TV's endpoint and in its details, and syncs with the device.
    func learnTVMac(_ id: String, mac: String) {
        guard let index = devices.firstIndex(where: { $0.id == id }), devices[index].details["tv.wifi_mac"] != mac else { return }
        devices[index].endpoints = devices[index].endpoints.map { endpoint in
            if case let .samsungTv(ip, tvId, _) = endpoint { return .samsungTv(ip: ip, id: tvId, mac: mac) }
            return endpoint
        }
        devices[index].details["tv.wifi_mac"] = mac
        // It just answered; the newest sighting is what account sync keeps endpoints from.
        devices[index].lastSeen = Self.now
        edited()
    }

    /// The TV ties a pairing to the device it allowed, so the token stays on this iPhone: it's
    /// never synced, and never replaced by the Mac's.
    func setTVToken(_ id: String, _ token: String) {
        guard let index = devices.firstIndex(where: { $0.id == id }) else { return }
        devices[index].tvToken = token
        save()
    }

    // MARK: Reachability

    func checkAll(ids: [String]? = nil) async {
        let targets = devices.filter { ids?.contains($0.id) ?? true }
        guard !targets.isEmpty else { return }
        if ids == nil { checking = true }
        for device in targets { checks[device.id] = .checking }
        await Reach.check(targets, cloud: cloud) { [weak self] report, relocated in
            guard let self, let index = self.devices.firstIndex(where: { $0.id == report.id }) else { return }
            self.checks[report.id] = .done(report)
            if report.status == .reachable {
                self.devices[index].lastSeen = UInt64(Date().timeIntervalSince1970)
                self.devices[index].lastAddress = report.address ?? self.devices[index].lastAddress
            }
            if let relocated { self.devices[index].endpoints = relocated }
            self.save()
        }
        if ids == nil {
            checking = false
            lastCheck = Date()
        }
    }

    // MARK: SmartThings

    /// Checks a personal token and keeps it. It works straight away, for 24 hours.
    func connectSmartThings(token: String) async throws {
        let token = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !token.isEmpty else { throw CloudError.api(0, "Paste a token first") }
        _ = try await SmartThingsClient(token: token).devices()
        storeToken(token, savedAt: Date(), oauth: nil)
        scheduleAccountSync()
        await refreshCloud()
    }

    /// The SmartThings app to sign in with, when there is one already: the one made or brought
    /// here before, or the one behind a token synced from another device.
    var knownSmartThingsApp: SmartThingsOAuth.App? {
        savedApp ?? grant.map { SmartThingsOAuth.App(clientId: $0.clientId, clientSecret: $0.clientSecret, ours: false) }
    }

    /// Thingscan's own SmartThings app: one already known, or a new one made with `pat`, which
    /// needs the Apps scope.
    func smartThingsApp(pat: String) async throws -> SmartThingsOAuth.App {
        if let known = knownSmartThingsApp { return known }
        let app = try await SmartThingsOAuth.createApp(pat: pat)
        keep(app)
        return app
    }

    /// Forgets a saved app SmartThings no longer accepts, so the next connect makes another.
    func forgetSmartThingsApp() {
        Keychain.delete(Self.appAccount)
    }

    /// An app the user made themselves, e.g. with `smartthings apps:create`.
    func useOwnApp(clientId: String, clientSecret: String) throws -> SmartThingsOAuth.App {
        let id = clientId.trimmingCharacters(in: .whitespacesAndNewlines)
        let secret = clientSecret.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !id.isEmpty, !secret.isEmpty else { throw CloudError.api(0, "Enter the client ID and the client secret") }
        let app = SmartThingsOAuth.App(clientId: id, clientSecret: secret, ours: false)
        keep(app)
        return app
    }

    /// Ends the SmartThings sign-in: trades its code for a token that renews itself, replacing
    /// any personal token.
    func finishSmartThingsSignIn(app: SmartThingsOAuth.App, code: String) async throws {
        let tokens = try await SmartThingsOAuth.exchange(code: code, app: app)
        let now = Self.now
        storeToken(tokens.accessToken, savedAt: Date(timeIntervalSince1970: TimeInterval(now)), oauth: tokens.grant(for: app, now: now), own: true)
        scheduleAccountSync()
        await refreshCloud()
    }

    private var savedApp: SmartThingsOAuth.App? {
        Keychain.get(Self.appAccount).flatMap { try? JSONDecoder().decode(SmartThingsOAuth.App.self, from: Data($0.utf8)) }
    }

    private func keep(_ app: SmartThingsOAuth.App) {
        if let data = try? JSONEncoder().encode(app) { Keychain.set(String(decoding: data, as: UTF8.self), for: Self.appAccount) }
    }

    /// Whether this phone renews SmartThings access. Paired straight with a Mac and not signed
    /// in, the Mac renews the token it sent and the phone takes what it sends; renewing here too
    /// would break the Mac's copy, since each renewal retires the one before. A token signed in
    /// for here the Mac doesn't have, so the phone renews that one.
    private var mayRenew: Bool { accountSession != nil || pairing == nil || ownGrant }

    /// Renews the access token when it has less than six hours left, or now with `force`.
    func renewCloudIfNeeded(force: Bool = false) async {
        if let renewing {
            await renewing.value
            return
        }
        guard let grant, mayRenew, force || grant.expiresAt < Self.now + Self.renewAhead else { return }
        let task = Task { await renew(grant) }
        renewing = task
        await task.value
        renewing = nil
    }

    private func renew(_ grant: OAuthGrant) async {
        do {
            let tokens = try await SmartThingsOAuth.refresh(grant)
            let now = Self.now
            let app = SmartThingsOAuth.App(clientId: grant.clientId, clientSecret: grant.clientSecret, ours: false)
            storeToken(tokens.accessToken, savedAt: Date(timeIntervalSince1970: TimeInterval(now)), oauth: tokens.grant(for: app, now: now), own: ownGrant)
            // The other devices' copy stopped working; pass the new one on.
            scheduleAccountSync()
        } catch CloudError.grant {
            // A refresh token works once: another device most likely renewed first, and its
            // token comes with a sync. This one is spent, so the Mac's may replace it now.
            setOwnGrant(false)
            await syncForToken()
            if self.grant?.refreshToken == grant.refreshToken {
                cloudError = "SmartThings access was renewed on another device or revoked. Sync again, or connect SmartThings again."
            }
        } catch {
            // Offline, or SmartThings is down: the token in hand may still work; try again later.
            if grant.expiresAt <= Self.now { cloudError = describe(error) }
        }
    }

    /// For a token SmartThings refused: the newer one another device synced, or one renewed here.
    private func freshToken(after failed: String) async -> String? {
        let current = { Keychain.get(Self.tokenAccount).flatMap { $0 == failed ? nil : $0 } }
        if let token = current() { return token }
        guard cloudRenews else { return nil }
        if let recovering { return await recovering.value }
        let task = Task { () -> String? in
            await syncForToken()
            if let token = current() { return token }
            await renewCloudIfNeeded(force: true)
            return current()
        }
        recovering = task
        let token = await task.value
        recovering = nil
        return token
    }

    private func syncForToken() async {
        if accountSession != nil {
            await syncAccount()
        } else if pairing != nil {
            await syncNow()
        }
    }

    /// The error to show for a failed SmartThings request.
    private func describe(_ error: Error) -> String {
        guard (error as? CloudError) == .unauthorized else { return error.localizedDescription }
        return cloudRenews
            ? "SmartThings didn't accept Thingscan's access. Sync again, or connect SmartThings again."
            : "SmartThings didn't accept the token: it's wrong or expired (personal tokens made since 30 Dec 2024 last 24 hours)."
    }

    /// Removes the token. The time is kept, so account sync passes the disconnect on.
    func disconnectSmartThings() {
        clearToken(at: Date())
        for index in devices.indices where devices[index].cloudId != nil {
            detach(&devices[index])
            devices[index].editedAt = Self.now
        }
        edited()
    }

    private func clearToken(at date: Date) {
        Keychain.delete(Self.tokenAccount)
        Keychain.delete(Self.grantAccount)
        hasCloudToken = false
        cloudRenews = false
        setOwnGrant(false)
        tokenHint = nil
        tokenSavedAt = date
        UserDefaults.standard.set(date, forKey: "tokenSavedAt")
        cloudDevices = []
        cloudError = nil
    }

    /// The token itself, for the account screen's reveal and copy.
    func revealToken() -> String? { Keychain.get(Self.tokenAccount) }

    func refreshCloud() async {
        guard let cloud else { return }
        do {
            cloudDevices = try await cloud.devices()
            cloudError = nil
            autoLink()
        } catch {
            cloudError = describe(error)
        }
    }

    func noteCloudError(_ error: Error) {
        if (error as? CloudError) == .unauthorized { cloudError = describe(error) }
    }

    func link(_ id: String, to cloudId: String?) {
        guard let index = devices.firstIndex(where: { $0.id == id }) else { return }
        if let cloudId, let cloud = cloudDevices.first(where: { $0.deviceId == cloudId }) {
            attach(&devices[index], cloud)
        } else {
            detach(&devices[index])
        }
        devices[index].editedAt = Self.now
        edited()
    }

    /// `own`: signed in for on this iPhone, or renewed from such a token, rather than synced.
    private func storeToken(_ token: String, savedAt: Date, oauth: OAuthGrant?, own: Bool = false) {
        Keychain.set(token, for: Self.tokenAccount)
        if let oauth, let data = try? JSONEncoder().encode(oauth) {
            Keychain.set(String(decoding: data, as: UTF8.self), for: Self.grantAccount)
        } else {
            Keychain.delete(Self.grantAccount)
        }
        hasCloudToken = true
        cloudRenews = oauth != nil
        setOwnGrant(own && oauth != nil)
        tokenHint = String(token.suffix(4))
        tokenSavedAt = savedAt
        UserDefaults.standard.set(savedAt, forKey: "tokenSavedAt")
        cloudError = nil
    }

    private func setOwnGrant(_ own: Bool) {
        ownGrant = own
        UserDefaults.standard.set(own, forKey: "ownGrant")
    }

    /// Links saved devices to the SmartThings device with the same OCF id or, failing that, the
    /// only one of the same model (a TV found over UPnP has no OCF id), like the Mac does.
    private func autoLink() {
        var changed = false
        for index in devices.indices where devices[index].cloudId == nil {
            if let match = cloudDevices.first(where: { cloudIds($0).contains { devices[index].ids.contains($0) } }) ?? onlyOneOfModel(index) {
                attach(&devices[index], match)
                devices[index].editedAt = Self.now
                changed = true
            }
        }
        if changed { edited() }
    }

    /// The SmartThings device of the same model, when neither side has another unlinked one of it.
    private func onlyOneOfModel(_ index: Int) -> CloudDevice? {
        guard let model = devices[index].model?.lowercased(), !model.isEmpty else { return nil }
        let linked = Set(devices.compactMap(\.cloudId))
        let candidates = cloudDevices.filter { $0.model?.lowercased() == model && !linked.contains($0.deviceId) }
        let rivals = devices.filter { $0.cloudId == nil && $0.model?.lowercased() == model }
        return candidates.count == 1 && rivals.count == 1 ? candidates[0] : nil
    }

    private func cloudIds(_ cloud: CloudDevice) -> [String] {
        ["st:\(cloud.deviceId)"] + (cloud.di.map { ["ocf:\($0)"] } ?? [])
    }

    private func attach(_ device: inout SavedDevice, _ cloud: CloudDevice) {
        detach(&device)
        device.cloudId = cloud.deviceId
        for id in cloudIds(cloud) where !device.ids.contains(id) { device.ids.append(id) }
        device.endpoints.append(.cloud(deviceId: cloud.deviceId))
        device.manufacturer = device.manufacturer ?? cloud.manufacturer
        device.model = device.model ?? cloud.model
    }

    private func detach(_ device: inout SavedDevice) {
        if let old = device.cloudId { device.ids.removeAll { $0 == "st:\(old)" } }
        device.cloudId = nil
        device.endpoints.removeAll { if case .cloud = $0 { true } else { false } }
    }

    static func kind(ofCategory category: String?) -> Kind {
        switch category ?? "" {
        case "Television", "TV": .tv
        case "Hub", "Bridges": .hub
        case "AirConditioner", "Washer", "Dryer", "Refrigerator", "Dishwasher", "Oven", "Range", "Microwave", "RobotCleaner",
             "AirPurifier", "Humidifier", "Dehumidifier", "Cooktop", "KimchiRefrigerator", "WaterHeater", "ClothingCareMachine": .appliance
        default: .other
        }
    }

    // MARK: Mac

    func pair(with mac: MacEndpoint, code: String) async throws {
        let digits = code.filter(\.isNumber)
        guard digits.count == 6 else { throw SyncError.message("Enter the 6-digit code shown on the Mac.") }
        pairing = try await MacSync.pair(with: mac, code: digits, phoneName: UIDevice.current.name)
        await syncNow()
        // The Mac usually brings the SmartThings token along.
        if hasCloudToken { await refreshCloud() }
    }

    func syncNow() async {
        guard let pairing, !syncing else { return }
        syncing = true
        defer { syncing = false }
        do {
            let (payload, current) = try await MacSync.sync(pairing)
            self.pairing = current
            merge(payload)
            lastSync = Date()
            UserDefaults.standard.set(lastSync, forKey: "lastSync")
            syncError = nil
        } catch {
            syncError = error.localizedDescription
        }
    }

    func unpair() {
        MacSync.forget()
        pairing = nil
        lastSync = nil
        syncError = nil
        UserDefaults.standard.removeObject(forKey: "lastSync")
    }

    /// The Mac's devices come in (or update) here; the phone keeps its own renames and its
    /// own added devices. Devices the Mac removed leave the phone too.
    func merge(_ payload: SyncPayload) {
        var kept = devices.filter { local in
            !local.syncedFromMac || payload.devices.contains { $0.id == local.id || $0.sharesIdentity(with: local.ids) }
        }
        for var incoming in payload.devices {
            incoming.syncedFromMac = true
            if let index = kept.firstIndex(where: { $0.id == incoming.id || $0.sharesIdentity(with: incoming.ids) }) {
                let local = kept[index]
                incoming.id = local.id
                if local.editedOnPhone {
                    incoming.name = local.name
                    incoming.room = local.room
                    incoming.editedOnPhone = true
                }
                // This iPhone's own TV pairing; one copied from the Mac (older Macs sent theirs)
                // only makes the TV ask again.
                incoming.tvToken = local.tvToken == incoming.tvToken ? nil : local.tvToken
                kept[index] = incoming
            } else {
                kept.append(incoming)
            }
        }
        devices = kept
        // A token signed in for here stays: the phone renews it without the Mac.
        if let token = payload.smartthings, !ownGrant {
            let savedAt = token.savedAt.map { Date(timeIntervalSince1970: TimeInterval($0)) } ?? Date()
            if !hasCloudToken || (tokenSavedAt ?? .distantPast) < savedAt {
                storeToken(token.token, savedAt: savedAt, oauth: token.oauth)
            }
        }
        save()
    }

    // MARK: Thingscan account

    func signUp(server: String, email: String, password: String) async throws {
        let session = try await ThingscanAccount.signUp(server: server, email: email, password: password, device: UIDevice.current.name)
        startAccount(session)
        await syncAccount()
    }

    func signIn(server: String, email: String, password: String) async throws {
        let session = try await ThingscanAccount.signIn(server: server, email: email, password: password, device: UIDevice.current.name)
        startAccount(session)
        await syncAccount()
    }

    private func startAccount(_ session: AccountSession) {
        accountSession = session
        accountError = nil
        remoteVault = nil
        lastAccountSync = nil
        UserDefaults.standard.set(session.server, forKey: "accountServer")
    }

    /// Signs out here. The devices stay on the phone; they just stop syncing.
    func signOut() async {
        guard let session = accountSession else { return }
        accountSession = nil
        accountError = nil
        remoteVault = nil
        lastAccountSync = nil
        UserDefaults.standard.removeObject(forKey: "lastAccountSync")
        await ThingscanAccount.signOut(session)
    }

    /// Reads the vault, merges it with what's here, and writes it back when that adds anything.
    func syncAccount() async {
        guard accountSession != nil else { return }
        if accountSyncing {
            syncAgain = true
            return
        }
        accountSyncing = true
        defer { accountSyncing = false }
        repeat {
            syncAgain = false
            guard let session = accountSession else { return }
            do {
                try await syncRounds(session)
                lastAccountSync = Date()
                UserDefaults.standard.set(lastAccountSync, forKey: "lastAccountSync")
                accountError = nil
            } catch AccountError.signedOut {
                ThingscanAccount.forget()
                accountSession = nil
                remoteVault = nil
                accountError = AccountError.signedOut.localizedDescription
            } catch {
                accountError = error.localizedDescription
            }
        } while syncAgain
    }

    private func syncRounds(_ session: AccountSession) async throws {
        for _ in 0..<4 {
            let fetched = try await ThingscanAccount.fetch(session, known: remoteVault?.version)
            var remote: VaultDoc
            if let doc = fetched.doc {
                remote = doc
            } else if let cached = remoteVault, cached.version == fetched.version {
                remote = cached.doc
            } else {
                remoteVault = nil
                continue
            }
            remote.devices = remote.devices.map(Self.withoutTVToken) // older vaults carried them
            remoteVault = (fetched.version, remote)
            let local = VaultDoc(devices: devices.map(Self.withoutTVToken), removed: removed, smartthings: tokenRecord)
            let merged = ThingscanAccount.merge(local: local, remote: remote)
            apply(merged, over: local)
            if ThingscanAccount.sameForVault(merged, remote) { return }
            if let version = try await ThingscanAccount.push(session, merged, baseVersion: fetched.version) {
                remoteVault = (version, merged)
                return
            }
            remoteVault = nil // another device wrote first: read it and merge again
        }
        throw AccountError.message("Other devices kept changing your account; try again.")
    }

    private var tokenRecord: TokenRecord? {
        guard let tokenSavedAt else { return nil }
        let token = Keychain.get(Self.tokenAccount)
        return TokenRecord(token: token, savedAt: UInt64(tokenSavedAt.timeIntervalSince1970), oauth: token == nil ? nil : grant)
    }

    private func apply(_ merged: VaultDoc, over local: VaultDoc) {
        if !ThingscanAccount.sameForVault(merged, local) || merged.devices.map(\.lastSeen) != local.devices.map(\.lastSeen) {
            // What only this iPhone keeps: its own TV pairings and the direct-pairing flags.
            let mine = Dictionary(devices.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
            devices = merged.devices.map { device in
                var device = device
                if let local = mine[device.id] {
                    device.tvToken = local.tvToken
                    device.syncedFromMac = local.syncedFromMac
                    device.editedOnPhone = local.editedOnPhone
                }
                return device
            }
            removed = merged.removed
            for id in checks.keys where !devices.contains(where: { $0.id == id }) { checks[id] = nil }
            save()
        }
        guard merged.smartthings != local.smartthings, let record = merged.smartthings else { return }
        let date = Date(timeIntervalSince1970: TimeInterval(record.savedAt))
        if let token = record.token {
            storeToken(token, savedAt: date, oauth: record.oauth)
            Task { await refreshCloud() }
        } else {
            clearToken(at: date)
        }
    }

    /// While the app is open, look for changes from other devices every minute.
    func setActive(_ active: Bool) {
        poller?.cancel()
        poller = nil
        guard active else { return }
        poller = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 60_000_000_000)
                guard !Task.isCancelled else { return }
                await self?.syncAccount()
                await self?.renewCloudIfNeeded()
            }
        }
    }

    /// A change the user made: save it, and sync it shortly after.
    private func edited() {
        save()
        scheduleAccountSync()
    }

    private func scheduleAccountSync() {
        guard accountSession != nil else { return }
        pendingSync?.cancel()
        pendingSync = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            guard !Task.isCancelled else { return }
            await self?.syncAccount()
        }
    }

    private static var now: UInt64 { UInt64(Date().timeIntervalSince1970) }

    /// TV pairings stay out of the vault: the TV only honours a token on the device it gave it to.
    private static func withoutTVToken(_ device: SavedDevice) -> SavedDevice {
        var device = device
        device.tvToken = nil
        return device
    }

    // MARK: Storage

    private func load() {
        if let data = try? Data(contentsOf: file) {
            devices = (try? JSONDecoder().decode([SavedDevice].self, from: data)) ?? []
        }
        if let data = try? Data(contentsOf: removedFile) {
            removed = (try? JSONDecoder().decode([RemovedDevice].self, from: data)) ?? []
        }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(devices) {
            try? data.write(to: file, options: [.atomic, .completeFileProtection])
        }
        if let data = try? JSONEncoder().encode(removed) {
            try? data.write(to: removedFile, options: [.atomic, .completeFileProtection])
        }
    }

    private func clean(_ text: String?) -> String? {
        let trimmed = text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return trimmed.isEmpty ? nil : trimmed
    }
}
