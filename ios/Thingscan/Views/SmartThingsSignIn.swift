import SwiftUI
import WebKit

/// SmartThings' own sign-in page, for Thingscan's SmartThings app. SmartThings then sends the
/// page to httpbin.org with a code; the web view stops there and hands the code over, so
/// nothing is loaded from httpbin.org. (A system sign-in sheet can't catch an https address
/// without Associated Domains, which a free team doesn't get.)
struct SmartThingsSignInSheet: View {
    @Environment(\.dismiss) private var dismiss
    let app: SmartThingsOAuth.App
    let done: (Result<String, Error>) -> Void
    @State private var state = SmartThingsOAuth.randomHex(16)
    @State private var loading = true

    var body: some View {
        NavigationStack {
            SignInWebView(url: SmartThingsOAuth.authorizeURL(app: app, state: state), state: state, loading: $loading) { result in
                done(result)
                dismiss()
            }
            .overlay { if loading { ProgressView().tint(Theme.mutedStrong) } }
            .navigationTitle("Sign in to SmartThings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
    }
}

private struct SignInWebView: UIViewRepresentable {
    let url: URL
    let state: String
    @Binding var loading: Bool
    let finish: (Result<String, Error>) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeUIView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // Signs in for this once; nothing of the Samsung session stays on the phone.
        configuration.websiteDataStore = .nonPersistent()
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.load(URLRequest(url: url))
        return view
    }

    func updateUIView(_ view: WKWebView, context: Context) {}

    final class Coordinator: NSObject, WKNavigationDelegate {
        private let parent: SignInWebView
        private var finished = false

        init(_ parent: SignInWebView) { self.parent = parent }

        /// Stops at the redirect address and hands over its code (or SmartThings' refusal).
        private func intercept(_ url: URL?) -> Bool {
            guard let url, let result = SmartThingsOAuth.callback(url, state: parent.state) else { return false }
            if !finished {
                finished = true
                parent.finish(result.mapError { $0 as Error })
            }
            return true
        }

        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction, decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
            decisionHandler(intercept(action.request.url) ? .cancel : .allow)
        }

        func webView(_ webView: WKWebView, didReceiveServerRedirectForProvisionalNavigation navigation: WKNavigation!) {
            if intercept(webView.url) { webView.stopLoading() }
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            parent.loading = false
            _ = intercept(webView.url)
        }

        func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
            parent.loading = false
        }

        func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
            parent.loading = false
            // Stopping the redirect above, or one page replacing another, isn't a failure.
            let error = error as NSError
            if error.domain == NSURLErrorDomain, error.code != NSURLErrorCancelled, !finished {
                finished = true
                parent.finish(.failure(CloudError.network(error.localizedDescription)))
            }
        }
    }
}
