import Foundation
import Network

/// DNS-SD browsing with Network.framework. The types must be listed under NSBonjourServices.
enum Bonjour {
    struct Service: Hashable {
        var name: String
        var type: String
        var domain: String
        var txt: [String: String]
        /// `Name._type._tcp.local.`, as the Mac stores it.
        var fullname: String { "\(name).\(type).\(domain.hasSuffix(".") ? domain : domain + ".")" }
    }

    static func browse(_ type: String, for seconds: TimeInterval) async -> [Service] {
        let bare = type.replacingOccurrences(of: ".local.", with: "").replacingOccurrences(of: ".local", with: "")
        return await withCheckedContinuation { continuation in
            let browser = NWBrowser(for: .bonjourWithTXTRecord(type: bare, domain: "local."), using: .tcp)
            let state = BrowseState()
            let finish = {
                state.lock.lock()
                defer { state.lock.unlock() }
                guard !state.finished else { return }
                state.finished = true
                browser.cancel()
                continuation.resume(returning: state.found)
            }
            browser.browseResultsChangedHandler = { results, _ in
                let services = results.compactMap { result -> Service? in
                    guard case let .service(name, type, domain, _) = result.endpoint else { return nil }
                    var txt: [String: String] = [:]
                    if case let .bonjour(record) = result.metadata {
                        for (key, entry) in record.dictionary { txt[key] = entry }
                    }
                    return Service(name: name, type: type, domain: domain, txt: txt)
                }
                state.lock.lock()
                state.found = services
                state.lock.unlock()
            }
            browser.stateUpdateHandler = { browserState in
                if case .failed = browserState { finish() }
            }
            browser.start(queue: .global(qos: .userInitiated))
            DispatchQueue.global().asyncAfter(deadline: .now() + seconds) { finish() }
        }
    }

    /// What the browser has seen so far; every access holds `lock`.
    private final class BrowseState: @unchecked Sendable {
        let lock = NSLock()
        var found: [Service] = []
        var finished = false
    }
}
