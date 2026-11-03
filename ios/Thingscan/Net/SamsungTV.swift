import Foundation

/// A Samsung Tizen TV's local APIs: device info on :8001 and the remote-control WebSocket.
enum SamsungTV {
    struct Info {
        var name: String?
        var model: String?
        var id: String?
        var mac: String?
        var power: String?
    }

    static func info(ip: String, timeout: TimeInterval = 2) async -> Info? {
        guard let url = URL(string: "http://\(ip):8001/api/v2/") else { return nil }
        var request = URLRequest(url: url)
        request.timeoutInterval = timeout
        guard let (data, _) = try? await URLSession.shared.data(for: request),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let device = json["device"] as? [String: Any] ?? [:]
        return Info(
            name: device["name"] as? String ?? json["name"] as? String,
            model: device["modelName"] as? String,
            id: device["id"] as? String ?? json["id"] as? String,
            mac: device["wifiMac"] as? String,
            power: device["PowerState"] as? String
        )
    }
}

enum TVError: LocalizedError {
    case unreachable, refused, noAnswer, closed

    var errorDescription: String? {
        switch self {
        case .unreachable: "The TV didn't answer; it may be off."
        case .refused: "The TV refused this remote. Allow Thingscan in the TV's device connection settings."
        case .noAnswer: "The TV didn't answer. Choose Allow on the TV, then try again."
        case .closed: "The TV closed the connection."
        }
    }
}

/// The TV's remote channel: wss://<tv>:8002/api/v2/channels/samsung.remote.control. The
/// first connection makes the TV ask whether to allow this remote; the token it then sends
/// skips the question next time.
final class TVRemote: NSObject, URLSessionDelegate, URLSessionTaskDelegate {
    private let ip: String
    private var session: URLSession!
    private var task: URLSessionWebSocketTask?

    init(ip: String) {
        self.ip = ip
        super.init()
        session = URLSession(configuration: .ephemeral, delegate: self, delegateQueue: nil)
    }

    deinit { close() }

    /// Connects and waits for the TV's verdict. Returns a new token when the TV issued one.
    func connect(name: String, token: String?, wait: TimeInterval) async throws -> String? {
        let encoded = Data(name.utf8).base64EncodedString().addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? ""
        var query = "name=\(encoded)"
        if let token { query += "&token=\(token)" }
        let urls = [
            URL(string: "wss://\(ip):8002/api/v2/channels/samsung.remote.control?\(query)")!,
            URL(string: "ws://\(ip):8001/api/v2/channels/samsung.remote.control?name=\(encoded)")!,
        ]
        for url in urls {
            let started = Date()
            do {
                return try await open(url, wait: wait)
            } catch TVError.unreachable where Date().timeIntervalSince(started) < wait - 0.5 {
                // Refused straight away: an older TV without the secure port. Try the next one.
                continue
            }
        }
        throw TVError.unreachable
    }

    private func open(_ url: URL, wait: TimeInterval) async throws -> String? {
        let task = session.webSocketTask(with: url)
        task.resume()
        self.task = task
        // receive() ignores task cancellation, so the deadline closes the socket instead.
        let deadline = Task {
            try await Task.sleep(nanoseconds: UInt64(wait * 1_000_000_000))
            task.cancel(with: .goingAway, reason: nil)
        }
        defer { deadline.cancel() }
        do {
            while true {
                guard case let .string(text) = try await task.receive(),
                      let event = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any] else { continue }
                switch event["event"] as? String {
                case "ms.channel.connect":
                    return (event["data"] as? [String: Any])?["token"] as? String
                case "ms.channel.unauthorized":
                    task.cancel(with: .normalClosure, reason: nil)
                    throw TVError.refused
                default:
                    continue
                }
            }
        } catch TVError.refused {
            throw TVError.refused
        } catch {
            task.cancel(with: .normalClosure, reason: nil)
            // The handshake finished but the TV stayed quiet: its Allow prompt went unanswered.
            throw task.response == nil ? TVError.unreachable : TVError.noAnswer
        }
    }

    var isOpen: Bool { task?.state == .running }

    func click(_ key: String) async throws { try await send(command("Click", key)) }

    /// Puts a key down without letting go. A single press held for a while reads as a short one;
    /// sent again and again until release(_:), the way a real remote repeats a held key, it's a
    /// long press: a few seconds of KEY_POWER turns a Frame TV fully off.
    func press(_ key: String) async throws { try await send(command("Press", key)) }

    /// Lets go of a key put down with press(_:).
    func release(_ key: String) async throws { try await send(command("Release", key)) }

    /// Moves the TV's on-screen pointer by a relative amount (the touchpad).
    func movePointer(dx: Int, dy: Int) async throws {
        try await send(["method": "ms.remote.control", "params": [
            "Cmd": "Move", "Position": ["x": dx, "y": dy, "Time": "0"], "TypeOfRemote": "ProcessMouseDevice",
        ]])
    }

    /// Clicks where the pointer is.
    func pointerClick() async throws {
        try await send(["method": "ms.remote.control", "params": ["Cmd": "LeftClick", "TypeOfRemote": "ProcessMouseDevice"]])
    }

    func launch(appId: String) async throws {
        try await send([
            "method": "ms.channel.emit",
            "params": ["event": "ed.apps.launch", "to": "host", "data": ["action_type": "DEEP_LINK", "appId": appId, "metaTag": ""]],
        ])
    }

    func close() {
        task?.cancel(with: .normalClosure, reason: nil)
        task = nil
    }

    private func command(_ cmd: String, _ key: String) -> [String: Any] {
        ["method": "ms.remote.control", "params": ["Cmd": cmd, "DataOfCmd": key, "Option": "false", "TypeOfRemote": "SendRemoteKey"]]
    }

    private func send(_ payload: [String: Any]) async throws {
        guard let task, task.state == .running else { throw TVError.closed }
        let data = try JSONSerialization.data(withJSONObject: payload)
        try await task.send(.string(String(decoding: data, as: UTF8.self)))
    }

    // The TV presents a certificate from Samsung's private SmartViewSDK CA; trust it for this
    // TV's address only. The pairing token is what authenticates the connection.
    func urlSession(_ session: URLSession, didReceive challenge: URLAuthenticationChallenge, completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        trust(challenge, completionHandler)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge, completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        trust(challenge, completionHandler)
    }

    private func trust(_ challenge: URLAuthenticationChallenge, _ completion: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        if challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
           challenge.protectionSpace.host == ip, let trust = challenge.protectionSpace.serverTrust {
            completion(.useCredential, URLCredential(trust: trust))
        } else {
            completion(.performDefaultHandling, nil)
        }
    }
}
