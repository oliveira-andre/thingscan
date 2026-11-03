import Foundation

/// The SmartThings cloud API, which controls appliances: their local OCF control resources
/// only accept the owning account's DTLS credentials.
enum CloudError: LocalizedError, Equatable {
    case unauthorized
    case forbidden(String)
    case api(Int, String)
    case network(String)
    /// SmartThings refused a refresh token or sign-in code: used already, or revoked.
    case grant(String)
    /// The personal token can't create Thingscan's own SmartThings app.
    case noAppsAccess

    var errorDescription: String? {
        switch self {
        case .unauthorized: "SmartThings didn't accept the token: it's wrong or has expired."
        case let .forbidden(m): "The token isn't allowed to do that (\(m)); it needs the Devices scopes."
        case let .api(status, m): "SmartThings said: \(m) (\(status))"
        case let .network(m): "Couldn't reach SmartThings: \(m)"
        case let .grant(m): "SmartThings refused the sign-in (\(m))."
        case .noAppsAccess: "This token can't set up Thingscan's own SmartThings app. Generate one with Apps ticked as well, and Thingscan keeps access by itself from then on."
        }
    }
}

struct SmartThingsClient {
    let token: String
    var base = "https://api.smartthings.com/v1"
    /// Given a token SmartThings refused, a fresher one (another device renewed it, or this
    /// phone just did), or nil. Tried once per request.
    var renew: (@Sendable (String) async -> String?)? = nil

    private func request(_ method: String, _ url: URL, body: Data? = nil) async throws -> JSONValue {
        do {
            return try await send(method, url, body: body, token: token)
        } catch CloudError.unauthorized {
            guard let renew, let fresh = await renew(token), fresh != token else { throw CloudError.unauthorized }
            return try await send(method, url, body: body, token: fresh)
        }
    }

    private func send(_ method: String, _ url: URL, body: Data?, token: String) async throws -> JSONValue {
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 15
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            request.httpBody = body
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw CloudError.network(error.localizedDescription)
        }
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        let value = (try? JSONDecoder().decode(JSONValue.self, from: data)) ?? .null
        let message = value["error"]?["message"]?.string ?? value["message"]?.string ?? "HTTP \(status)"
        switch status {
        case 200 ..< 300: return value
        case 401: throw CloudError.unauthorized
        case 403: throw CloudError.forbidden(message)
        default: throw CloudError.api(status, message)
        }
    }

    private func url(_ path: String) -> URL { URL(string: base + path)! }

    private func safe(_ id: String) -> String { id.filter { $0.isLetter || $0.isNumber || $0 == "-" } }

    /// Every device in the account, with `di` filled in for OCF devices.
    func devices() async throws -> [CloudDevice] {
        var out: [CloudDevice] = []
        var next: URL? = url("/devices")
        var pages = 0
        while let page = next, pages < 50 {
            let value = try await request("GET", page)
            out += (value["items"]?.array ?? []).compactMap(Self.parse)
            next = value["_links"]?["next"]?["href"]?.string.flatMap(URL.init(string:))
            pages += 1
        }
        return await withTaskGroup(of: (Int, String?).self) { group in
            for (index, device) in out.enumerated() where device.capabilities.contains("ocf") {
                group.addTask {
                    let status = try? await request("GET", url("/devices/\(safe(device.deviceId))/components/main/capabilities/ocf/status"))
                    return (index, status?["di"]?["value"]?.string)
                }
            }
            var filled = out
            for await (index, di) in group { filled[index].di = di }
            return filled
        }
    }

    static func parse(_ item: JSONValue) -> CloudDevice? {
        guard let id = item["deviceId"]?.string else { return nil }
        let main = item["components"]?.array?.first { $0["id"]?.string == "main" }
        let nonEmpty = { (v: JSONValue?) -> String? in v?.string.flatMap { $0.isEmpty ? nil : $0 } }
        return CloudDevice(
            deviceId: id,
            name: nonEmpty(item["label"]) ?? nonEmpty(item["name"]) ?? id,
            manufacturer: nonEmpty(item["manufacturerName"]) ?? nonEmpty(item["ocf"]?["manufacturerName"]),
            category: main?["categories"]?.array?.first?["name"]?.string,
            ocfType: nonEmpty(item["ocf"]?["ocfDeviceType"]),
            model: nonEmpty(item["ocf"]?["modelNumber"]).map { String($0.split(separator: "|").first ?? "") },
            capabilities: main?["capabilities"]?.array?.compactMap { $0["id"]?.string } ?? [],
            di: nil
        )
    }

    /// `components.main.<capability>.<attribute>.value`
    func status(_ deviceId: String) async throws -> JSONValue {
        try await request("GET", url("/devices/\(safe(deviceId))/status"))
    }

    /// ONLINE, OFFLINE or UNHEALTHY.
    func health(_ deviceId: String) async throws -> String {
        try await request("GET", url("/devices/\(safe(deviceId))/health"))["state"]?.string ?? "UNKNOWN"
    }

    func command(_ deviceId: String, capability: String, command: String, arguments: [Any] = [], component: String = "main") async throws {
        var body: [String: Any] = ["component": component, "capability": capability, "command": command]
        if !arguments.isEmpty { body["arguments"] = arguments }
        let data = try JSONSerialization.data(withJSONObject: ["commands": [body]])
        _ = try await request("POST", url("/devices/\(safe(deviceId))/commands"), body: data)
    }
}

/// Thingscan's own SmartThings app, an "OAuth-In" API-only app, so access renews itself:
/// personal tokens made since 30 Dec 2024 last 24 hours, and SmartThings has no lasting kind.
/// A personal token with the Apps scope creates the app once; signing in to SmartThings then
/// gives an access token (24 hours) and a refresh token (single use, 29 days if unused).
/// Each refresh replaces both, and the old access token stops working.
enum SmartThingsOAuth {
    /// SmartThings only redirects to https addresses; the sign-in view stops before loading it.
    static let redirectURI = "https://httpbin.org/get"
    static let scopes = ["r:devices:*", "w:devices:*", "x:devices:*", "r:locations:*"]
    static let authorizeURL = "https://api.smartthings.com/oauth/authorize"
    static let tokenURL = URL(string: "https://api.smartthings.com/oauth/token")!
    static let appsURL = URL(string: "https://api.smartthings.com/v1/apps")!

    /// The app's credentials. SmartThings shows the secret only when the app is created.
    struct App: Codable, Hashable {
        var clientId: String
        var clientSecret: String
        /// Created by Thingscan, so it has `scopes`. One brought by the user may not: its
        /// sign-in leaves the scope out and SmartThings offers the app's own.
        var ours: Bool
    }

    /// A token reply.
    struct Tokens: Equatable {
        var accessToken: String
        var refreshToken: String
        var expiresIn: UInt64

        /// The grant to keep, as of `now` (seconds).
        func grant(for app: App, now: UInt64) -> OAuthGrant {
            OAuthGrant(clientId: app.clientId, clientSecret: app.clientSecret, refreshToken: refreshToken, expiresAt: now + expiresIn)
        }
    }

    /// Creates the app with a personal token that has the Apps scope.
    static func createApp(pat: String) async throws -> App {
        let body: [String: Any] = [
            "appName": "thingscan-\(randomHex(8))",
            "displayName": "Thingscan",
            "description": "Lets Thingscan on your Mac and iPhone control your appliances.",
            "appType": "API_ONLY",
            "classifications": ["CONNECTED_SERVICE"],
            "singleInstance": true,
            "principalType": "LOCATION",
            "apiOnly": [String: Any](),
            "oauth": ["clientName": "Thingscan", "scope": scopes, "redirectUris": [redirectURI]],
        ]
        var request = URLRequest(url: appsURL)
        request.httpMethod = "POST"
        request.timeoutInterval = 20
        request.setValue("Bearer \(pat.trimmingCharacters(in: .whitespacesAndNewlines))", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (status, json) = try await call(request)
        switch status {
        case 200 ..< 300:
            guard let id = json["oauthClientId"]?.string, let secret = json["oauthClientSecret"]?.string else {
                throw CloudError.api(status, "the new app came without OAuth credentials")
            }
            return App(clientId: id, clientSecret: secret, ours: true)
        case 401, 403: throw CloudError.noAppsAccess
        default: throw CloudError.api(status, message(json, status))
        }
    }

    /// SmartThings' sign-in page for `app`, which comes back to `redirectURI` with a code.
    static func authorizeURL(app: App, state: String) -> URL {
        var query = "client_id=\(encode(app.clientId))&response_type=code&redirect_uri=\(encode(redirectURI))&state=\(encode(state))"
        if app.ours { query += "&scope=\(encode(scopes.joined(separator: " ")))" }
        return URL(string: "\(authorizeURL)?\(query)")!
    }

    /// The code from the sign-in redirect, once its state matches. Nil for any other address.
    static func callback(_ url: URL, state: String) -> Result<String, CloudError>? {
        guard url.host() == URL(string: redirectURI)?.host(),
              let items = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems else { return nil }
        let value = { (name: String) in items.first { $0.name == name }?.value }
        if let error = value("error") { return .failure(.grant(value("error_description") ?? error)) }
        guard let code = value("code"), !code.isEmpty else { return nil }
        guard value("state") == state else { return .failure(.grant("the answer didn't match this sign-in")) }
        return .success(code)
    }

    static func exchange(code: String, app: App) async throws -> Tokens {
        try await token(app.clientId, app.clientSecret, [
            ("grant_type", "authorization_code"), ("client_id", app.clientId), ("code", code), ("redirect_uri", redirectURI),
        ])
    }

    static func refresh(_ grant: OAuthGrant) async throws -> Tokens {
        try await token(grant.clientId, grant.clientSecret, [
            ("grant_type", "refresh_token"), ("client_id", grant.clientId), ("refresh_token", grant.refreshToken),
        ])
    }

    private static func token(_ clientId: String, _ secret: String, _ form: [(String, String)]) async throws -> Tokens {
        var request = URLRequest(url: tokenURL)
        request.httpMethod = "POST"
        request.timeoutInterval = 20
        request.setValue("Basic \(Data("\(clientId):\(secret)".utf8).base64EncodedString())", forHTTPHeaderField: "Authorization")
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = Data(form.map { "\($0.0)=\(encode($0.1))" }.joined(separator: "&").utf8)
        let (status, json) = try await call(request)
        if (200 ..< 300).contains(status), let tokens = parseTokens(json) { return tokens }
        if json["error"]?.string == "invalid_grant" { throw CloudError.grant(json["error_description"]?.string ?? "invalid_grant") }
        throw CloudError.api(status, message(json, status))
    }

    static func parseTokens(_ json: JSONValue) -> Tokens? {
        guard let access = json["access_token"]?.string, let refresh = json["refresh_token"]?.string else { return nil }
        return Tokens(accessToken: access, refreshToken: refresh, expiresIn: UInt64(max(0, json["expires_in"]?.double ?? 86_399)))
    }

    private static func call(_ request: URLRequest) async throws -> (Int, JSONValue) {
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            return ((response as? HTTPURLResponse)?.statusCode ?? 0, (try? JSONDecoder().decode(JSONValue.self, from: data)) ?? .null)
        } catch {
            throw CloudError.network(error.localizedDescription)
        }
    }

    private static func message(_ json: JSONValue, _ status: Int) -> String {
        json["error_description"]?.string ?? json["error"]?["message"]?.string ?? json["error"]?.string
            ?? json["message"]?.string ?? "HTTP \(status)"
    }

    /// Percent-encodes everything but unreserved characters (and `*`, as SmartThings' scopes have it).
    static func encode(_ text: String) -> String {
        text.addingPercentEncoding(withAllowedCharacters: unreserved) ?? text
    }

    private static let unreserved = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~*")

    static func randomHex(_ bytes: Int) -> String {
        (0 ..< bytes).map { _ in String(format: "%02x", UInt8.random(in: 0 ... 255)) }.joined()
    }
}
