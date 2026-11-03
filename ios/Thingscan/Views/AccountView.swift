import SwiftUI

/// The Thingscan account (sign in to sync through a Thingscan server), SmartThings (status,
/// the token masked with show and copy, replace, disconnect) and the direct pairing with the Mac.
struct AccountView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var revealed: String?
    @State private var copied = false
    @State private var showConnect = false
    @State private var showPair = false
    @State private var confirmDisconnect = false
    @State private var signIn: SignInMode?
    @State private var confirmSignOut = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    thingscan
                    smartThings
                    mac
                }
                .padding(16)
            }
            .background(Theme.canvas)
            .navigationTitle("Account")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } } }
            .sheet(isPresented: $showConnect) { ConnectSmartThingsSheet() }
            .sheet(isPresented: $showPair) { PairMacSheet() }
            .sheet(item: $signIn) { mode in AccountSignInSheet(mode: mode) }
        }
    }

    // MARK: Thingscan account

    private var thingscan: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "person.crop.circle").font(.system(size: 22)).foregroundStyle(Theme.primary)
                    .frame(width: 44, height: 44).background(Circle().fill(Theme.raised))
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.accountSession?.email ?? "Thingscan account")
                        .font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong).lineLimit(1)
                    HStack(spacing: 6) {
                        if model.accountSession != nil {
                            Circle().fill(model.accountError == nil ? Theme.up : Theme.pending).frame(width: 8, height: 8)
                        }
                        Text(accountLine)
                    }
                    .font(Theme.number(13, .regular))
                    .foregroundStyle(model.accountError == nil ? Theme.mutedStrong : Theme.pending)
                }
            }
            if let session = model.accountSession {
                if let error = model.accountError { Text(error).font(.system(size: 12)).foregroundStyle(Theme.pending) }
                Text("Your devices and SmartThings token sync through \(URL(string: session.server)?.host() ?? session.server) with your Mac and other iPhones, wherever they are.")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                HStack {
                    Button { Task { await model.refreshAll() } } label: { Label("Sync now", systemImage: "arrow.clockwise") }
                        .buttonStyle(SecondaryButtonStyle(compact: true))
                        .disabled(model.accountSyncing)
                    Button("Sign out", role: .destructive) { confirmSignOut = true }
                        .buttonStyle(SecondaryButtonStyle(compact: true, destructive: true))
                }
                .confirmationDialog("Sign out of Thingscan?", isPresented: $confirmSignOut, titleVisibility: .visible) {
                    Button("Sign out", role: .destructive) { Task { await model.signOut() } }
                } message: {
                    Text("Your devices stay on this iPhone; they just stop syncing.")
                }
            } else {
                Text("Sign in to keep your devices and SmartThings token in sync with your Mac and other iPhones, wherever they are.")
                    .font(.system(size: 13)).foregroundStyle(Theme.muted)
                HStack(spacing: 8) {
                    Button { signIn = .signIn } label: { Text("Sign in").frame(maxWidth: .infinity) }
                        .buttonStyle(PrimaryButtonStyle())
                    Button { signIn = .signUp } label: { Text("Create account").frame(maxWidth: .infinity) }
                        .buttonStyle(SecondaryButtonStyle())
                }
            }
        }
        .card(padding: 18)
    }

    private var accountLine: String {
        guard model.accountSession != nil else { return "Not signed in" }
        if model.accountSyncing { return "Syncing…" }
        if model.accountError != nil { return "Not syncing" }
        return model.lastAccountSync.map { "Synced \(Format.ago($0))" } ?? "Not synced yet"
    }

    // MARK: SmartThings

    private var smartThings: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "icloud").font(.system(size: 20)).foregroundStyle(Theme.primary)
                    .frame(width: 44, height: 44).background(Circle().fill(Theme.raised))
                VStack(alignment: .leading, spacing: 2) {
                    Text("SmartThings account").font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong)
                    HStack(spacing: 6) {
                        if model.hasCloudToken {
                            Circle().fill(model.cloudError == nil ? Theme.up : Theme.pending).frame(width: 8, height: 8)
                            Text(model.cloudError == nil ? "Connected · \(model.cloudDevices.count) devices" : model.cloudRenews ? "Needs attention" : "Token expired")
                        } else {
                            Text("Not connected")
                        }
                    }
                    .font(Theme.number(13, .regular))
                    .foregroundStyle(model.cloudError == nil ? Theme.mutedStrong : Theme.pending)
                }
            }
            if model.hasCloudToken {
                Divider().overlay(Theme.hairline)
                FieldLabel(text: model.cloudRenews ? "Access token" : "Personal access token")
                HStack(spacing: 6) {
                    Text(revealed ?? String(repeating: "•", count: 16) + (model.tokenHint ?? ""))
                        .font(.system(size: 13, design: .monospaced))
                        .foregroundStyle(Theme.text)
                        .lineLimit(revealed == nil ? 1 : nil)
                        .textSelection(.enabled)
                    Spacer(minLength: 4)
                    Button {
                        revealed = revealed == nil ? model.revealToken() : nil
                    } label: {
                        Image(systemName: revealed == nil ? "eye" : "eye.slash").foregroundStyle(Theme.mutedStrong).frame(width: 32, height: 32)
                    }
                    .accessibilityLabel(revealed == nil ? "Show token" : "Hide token")
                    Button {
                        if let token = model.revealToken() {
                            UIPasteboard.general.string = token
                            Haptics.tap()
                            copied = true
                            Task {
                                try? await Task.sleep(nanoseconds: 2_000_000_000)
                                copied = false
                            }
                        }
                    } label: {
                        Label(copied ? "Copied" : "Copy", systemImage: copied ? "checkmark" : "doc.on.doc")
                    }
                    .buttonStyle(PrimaryButtonStyle(compact: true))
                }
                .padding(.leading, 12)
                .padding(.trailing, 6)
                .padding(.vertical, 6)
                .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
                if model.cloudRenews {
                    Text("Renews by itself through Thingscan's SmartThings app" + (model.tokenSavedAt.map { " · renewed \(Format.ago($0))" } ?? "") + ".")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                } else {
                    Text((model.tokenSavedAt.map { "Saved \(Format.ago($0)). " } ?? "") + "Personal tokens made since December 2024 last 24 hours.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                }
                if let error = model.cloudError { Text(error).font(.system(size: 12)).foregroundStyle(Theme.pending) }
                Divider().overlay(Theme.hairline)
                if model.cloudRenews {
                    Button { showConnect = true } label: { Label("Connect again", systemImage: "key") }.foregroundStyle(Theme.text)
                } else {
                    // The same sheet takes a plain replacement token too.
                    Button { showConnect = true } label: { Label("Keep access automatically", systemImage: "arrow.triangle.2.circlepath") }
                        .foregroundStyle(Theme.primaryActive)
                }
                Button(role: .destructive) { confirmDisconnect = true } label: { Label("Disconnect", systemImage: "rectangle.portrait.and.arrow.right") }
                    .foregroundStyle(Theme.down)
                    .confirmationDialog("Disconnect SmartThings?", isPresented: $confirmDisconnect, titleVisibility: .visible) {
                        Button("Disconnect", role: .destructive) { model.disconnectSmartThings() }
                    } message: {
                        Text("Appliance controls stop working on this phone until you connect again.")
                    }
            } else {
                Text("Connect your SmartThings account to control air conditioners, washers and other appliances.")
                    .font(.system(size: 13)).foregroundStyle(Theme.muted)
                Button { showConnect = true } label: { Label("Connect SmartThings", systemImage: "link").frame(maxWidth: .infinity) }
                    .buttonStyle(PrimaryButtonStyle())
            }
        }
        .font(.system(size: 15))
        .card(padding: 18)
    }

    // MARK: Mac

    private var mac: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(spacing: 12) {
                Image(systemName: "laptopcomputer").font(.system(size: 18)).foregroundStyle(Theme.primary)
                    .frame(width: 44, height: 44).background(Circle().fill(Theme.raised))
                VStack(alignment: .leading, spacing: 2) {
                    Text(model.pairing?.macName ?? "Thingscan on your Mac").font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong)
                    Text(model.pairing == nil ? "Not paired" : model.syncing ? "Syncing…" : model.lastSync.map { "Synced \(Format.ago($0))" } ?? "Paired, not synced yet")
                        .font(Theme.number(13, .regular)).foregroundStyle(Theme.mutedStrong)
                }
            }
            if model.accountSession != nil {
                Text("Your Thingscan account keeps this iPhone in sync, so the direct pairing below isn't used while you're signed in.")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            if let pairing = model.pairing {
                if let error = model.syncError { Text(error).font(.system(size: 12)).foregroundStyle(Theme.pending) }
                Text("Devices you add or change on the Mac come over when this phone syncs, which happens every time you open Thingscan. Renames you make here are kept.")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                Text("\(pairing.host):\(String(pairing.port))").font(.system(size: 12, design: .monospaced)).foregroundStyle(Theme.muted)
                HStack {
                    Button {
                        Task { await model.refreshAll() }
                    } label: { Label("Sync now", systemImage: "arrow.clockwise") }
                        .buttonStyle(SecondaryButtonStyle(compact: true))
                        .disabled(model.syncing)
                    Button("Unpair", role: .destructive) { model.unpair() }.buttonStyle(SecondaryButtonStyle(compact: true, destructive: true))
                }
            } else {
                Text("Pair once to bring over the devices you added on the Mac and your SmartThings token. Everything travels encrypted.")
                    .font(.system(size: 13)).foregroundStyle(Theme.muted)
                Button { showPair = true } label: { Label("Pair with Mac", systemImage: "laptopcomputer").frame(maxWidth: .infinity) }
                    .buttonStyle(PrimaryButtonStyle())
            }
        }
        .card(padding: 18)
    }
}

enum SignInMode: String, Identifiable {
    case signIn, signUp
    var id: String { rawValue }
}

/// Signs in to a Thingscan account, or creates one.
struct AccountSignInSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State var mode: SignInMode
    @State private var server = UserDefaults.standard.string(forKey: "accountServer") ?? ""
    @State private var email = ""
    @State private var password = ""
    @State private var repeated = ""
    @State private var error: String?
    @State private var busy = false

    private var creating: Bool { mode == .signUp }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text(creating ? "Create a Thingscan account" : "Sign in to Thingscan")
                        .font(.system(size: 22, weight: .semibold)).foregroundStyle(Theme.strong)
                    Text(creating ? "Sync your devices with your Mac and other iPhones, from anywhere." : "Bring your devices to this iPhone and keep them in sync.")
                        .font(.system(size: 14)).foregroundStyle(Theme.muted)
                    FieldLabel(text: "Server")
                    FormField(placeholder: "sync.example.com", text: $server, monospaced: true)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    FieldLabel(text: "Email")
                    FormField(placeholder: "you@example.com", text: $email)
                        .keyboardType(.emailAddress)
                        .textContentType(.username)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                    FieldLabel(text: "Password")
                    FormField(placeholder: creating ? "At least 8 characters" : "Password", text: $password, secure: true)
                        .textContentType(creating ? .newPassword : .password)
                        .submitLabel(creating ? .next : .go)
                        .onSubmit { if !creating { submit() } }
                    if creating {
                        FieldLabel(text: "Repeat password")
                        FormField(placeholder: "Repeat password", text: $repeated, secure: true)
                            .textContentType(.newPassword)
                            .submitLabel(.go)
                            .onSubmit(submit)
                    }
                    Text("Your devices and SmartThings token are encrypted with your password before they leave this iPhone, so the server can't read them. That also means a forgotten password can't be reset.")
                        .font(.system(size: 12)).foregroundStyle(Theme.muted)
                    if let error { Text(error).font(.system(size: 13)).foregroundStyle(Theme.down) }
                    Button(action: submit) {
                        HStack {
                            if busy { ProgressView().tint(Theme.onPrimary) }
                            Text(creating ? "Create account" : "Sign in")
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(PrimaryButtonStyle())
                    .disabled(busy || server.isEmpty || email.isEmpty || password.isEmpty)
                    Button(creating ? "I already have an account" : "Create an account instead") {
                        mode = creating ? .signIn : .signUp
                        error = nil
                    }
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Theme.strong)
                    .frame(maxWidth: .infinity)
                }
                .padding(20)
            }
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .formSheet()
    }

    private func submit() {
        if creating, password != repeated {
            error = "The passwords don't match."
            return
        }
        busy = true
        error = nil
        Task {
            do {
                if creating {
                    try await model.signUp(server: server, email: email, password: password)
                } else {
                    try await model.signIn(server: server, email: email, password: password)
                }
                dismiss()
                await model.checkAll()
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }
}

/// Connects SmartThings. A personal token works straight away but only for 24 hours, so with
/// the Apps scope it also sets up Thingscan's own SmartThings app; signing in to SmartThings
/// through that app gives access that renews itself.
struct ConnectSmartThingsSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var token = ""
    @State private var error: String?
    @State private var note: String?
    @State private var busy = false
    /// The app to sign in with, once there is one.
    @State private var app: SmartThingsOAuth.App?
    @State private var signingIn: SmartThingsOAuth.App?
    @State private var ownApp = false
    @State private var clientId = ""
    @State private var clientSecret = ""

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text("Connect SmartThings").font(.system(size: 22, weight: .semibold)).foregroundStyle(Theme.strong)
                    Text("Samsung appliances only take commands from the account that owns them, so Thingscan controls them through your SmartThings account, the way the SmartThings app does. Your TV's remote works without it.")
                        .font(.system(size: 14)).foregroundStyle(Theme.muted)
                    if let app {
                        signInStep(app)
                    } else {
                        tokenStep
                    }
                    if let note { Text(note).font(.system(size: 13)).foregroundStyle(Theme.pending) }
                    if let error { Text(error).font(.system(size: 13)).foregroundStyle(Theme.down) }
                    if app == nil { ownAppStep }
                }
                .padding(20)
            }
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button(model.hasCloudToken ? "Done" : "Cancel") { dismiss() } } }
            .sheet(item: $signingIn) { app in
                SmartThingsSignInSheet(app: app) { result in finish(app, result) }
            }
        }
        .formSheet()
        .onAppear { app = app ?? model.knownSmartThingsApp }
    }

    private var tokenStep: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 8) {
                Text("1. Open **account.smartthings.com/tokens** and sign in.")
                Text("2. Generate a token. Under Devices, tick List all devices, See all devices and Control all devices. Under Apps, tick all of them too.")
                Text("3. Copy it and paste it below. Thingscan uses it once to set up its own SmartThings app, then you sign in to SmartThings.")
            }
            .font(.system(size: 14))
            .foregroundStyle(Theme.strong)
            .padding(14)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            Link(destination: URL(string: "https://account.smartthings.com/tokens")!) {
                Label("Open account.smartthings.com", systemImage: "arrow.up.forward.app")
            }
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(Theme.primaryActive)
            FieldLabel(text: "Personal access token")
            FormField(placeholder: "Paste the token here", text: $token, monospaced: true, secure: true)
            Text("SmartThings makes personal tokens last 24 hours. Through Thingscan's own app, access renews by itself and syncs to your other devices. Everything stays in this iPhone's Keychain.")
                .font(.system(size: 12)).foregroundStyle(Theme.muted)
            Button { Task { await connect() } } label: {
                HStack {
                    if busy { ProgressView().tint(Theme.onPrimary) }
                    Label("Connect", systemImage: "link")
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(busy || token.isEmpty)
        }
    }

    private func signInStep(_ app: SmartThingsOAuth.App) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Sign in to SmartThings with the account that has your appliances, and allow Thingscan. From then on, access renews by itself.")
                .font(.system(size: 14)).foregroundStyle(Theme.strong)
                .padding(14)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            Button { signingIn = app } label: {
                HStack {
                    if busy { ProgressView().tint(Theme.onPrimary) }
                    Label("Sign in to SmartThings", systemImage: "person.badge.key")
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(PrimaryButtonStyle())
            .disabled(busy)
            Button("Use a different app or token") {
                self.app = nil
                note = nil
                error = nil
            }
            .font(.system(size: 14)).foregroundStyle(Theme.mutedStrong)
        }
    }

    private var ownAppStep: some View {
        DisclosureGroup("Already have an OAuth app?", isExpanded: $ownApp) {
            VStack(alignment: .leading, spacing: 10) {
                Text("One you made with `smartthings apps:create` (OAuth-In App), with the redirect URI \(SmartThingsOAuth.redirectURI) and the Devices scopes.")
                    .font(.system(size: 12)).foregroundStyle(Theme.muted)
                FieldLabel(text: "Client ID")
                FormField(placeholder: "Client ID", text: $clientId, monospaced: true)
                FieldLabel(text: "Client secret")
                FormField(placeholder: "Client secret", text: $clientSecret, monospaced: true, secure: true)
                Button {
                    do {
                        let app = try model.useOwnApp(clientId: clientId, clientSecret: clientSecret)
                        self.app = app
                        error = nil
                        signingIn = app
                    } catch {
                        self.error = error.localizedDescription
                    }
                } label: { Label("Sign in with this app", systemImage: "person.badge.key").frame(maxWidth: .infinity) }
                    .buttonStyle(SecondaryButtonStyle())
                    .disabled(clientId.isEmpty || clientSecret.isEmpty)
            }
            .padding(.top, 10)
        }
        .font(.system(size: 14, weight: .semibold))
        .tint(Theme.mutedStrong)
    }

    /// Keeps the personal token (it works for today), then sets up the app to sign in with.
    private func connect() async {
        busy = true
        error = nil
        note = nil
        defer { busy = false }
        do {
            try await model.connectSmartThings(token: token)
        } catch {
            self.error = error.localizedDescription
            return
        }
        do {
            let app = try await model.smartThingsApp(pat: token)
            self.app = app
            signingIn = app
        } catch {
            note = "Connected, but only for 24 hours. \(error.localizedDescription)"
        }
    }

    private func finish(_ app: SmartThingsOAuth.App, _ result: Result<String, Error>) {
        switch result {
        case let .failure(error):
            self.error = error.localizedDescription
        case let .success(code):
            busy = true
            error = nil
            Task {
                do {
                    try await model.finishSmartThingsSignIn(app: app, code: code)
                    dismiss()
                } catch {
                    if case .api(401, _)? = error as? CloudError {
                        // SmartThings doesn't know this app (any more): set one up again.
                        model.forgetSmartThingsApp()
                        self.app = nil
                        self.error = "SmartThings didn't accept the app's client ID and secret. Connect again to set up a new one."
                    } else {
                        self.error = error.localizedDescription
                    }
                }
                busy = false
            }
        }
    }
}

extension SmartThingsOAuth.App: Identifiable {
    var id: String { clientId }
}

/// Finds Thingscan on the Mac over Bonjour (or takes its address), then takes the 6-digit code
/// the Mac shows.
struct PairMacSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var macs: [MacEndpoint] = []
    @State private var searching = true
    @State private var chosen: MacEndpoint?
    @State private var address = ""
    @State private var code = ""
    @State private var error: String?
    @State private var busy = false
    @FocusState private var codeFocused: Bool

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    Text("Pair with Mac").font(.system(size: 22, weight: .semibold)).foregroundStyle(Theme.strong)
                    Text("On the Mac, open Thingscan, click the person icon and choose **Pair an iPhone**. A 6-digit code appears.")
                        .font(.system(size: 14)).foregroundStyle(Theme.muted)
                    if let chosen {
                        HStack {
                            Image(systemName: "laptopcomputer")
                            Text(chosen.name).fontWeight(.semibold)
                            Spacer()
                            Button("Change") { self.chosen = nil; code = "" }.foregroundStyle(Theme.primaryActive)
                        }
                        .foregroundStyle(Theme.strong)
                        .padding(14)
                        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                        FieldLabel(text: "Code shown on the Mac")
                        TextField("000000", text: $code)
                            .keyboardType(.numberPad)
                            .textContentType(.oneTimeCode)
                            .focused($codeFocused)
                            .font(.system(size: 34, weight: .bold).monospacedDigit())
                            .kerning(6)
                            .multilineTextAlignment(.center)
                            .foregroundStyle(Theme.strong)
                            .frame(height: 64)
                            .background(RoundedRectangle(cornerRadius: 8).stroke(Theme.hairline))
                            .onChange(of: code) { _, value in code = String(value.filter(\.isNumber).prefix(6)) }
                        if let error { Text(error).font(.system(size: 13)).foregroundStyle(Theme.down) }
                        Button {
                            busy = true
                            error = nil
                            Task {
                                do {
                                    try await model.pair(with: chosen, code: code)
                                    await model.checkAll()
                                    dismiss()
                                } catch {
                                    self.error = error.localizedDescription
                                }
                                busy = false
                            }
                        } label: {
                            HStack {
                                if busy { ProgressView().tint(Theme.onPrimary) }
                                Text("Pair")
                            }
                            .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(PrimaryButtonStyle())
                        .disabled(busy || code.count != 6)
                    } else {
                        FieldLabel(text: "Macs on this Wi-Fi")
                        if searching {
                            HStack { ProgressView(); Text("Looking for Thingscan on your Mac…").foregroundStyle(Theme.muted) }.font(.system(size: 14))
                        } else if macs.isEmpty {
                            Text("None found. Make sure Thingscan is open on the Mac, or type the address shown under “iPhone app” in its account menu.")
                                .font(.system(size: 13)).foregroundStyle(Theme.muted)
                        }
                        ForEach(macs) { mac in
                            Button { choose(mac) } label: {
                                HStack {
                                    Image(systemName: "laptopcomputer")
                                    VStack(alignment: .leading) {
                                        Text(mac.name).fontWeight(.semibold)
                                        Text(mac.id).font(.system(size: 12, design: .monospaced)).foregroundStyle(Theme.muted)
                                    }
                                    Spacer()
                                    Image(systemName: "chevron.right").foregroundStyle(Theme.muted)
                                }
                                .foregroundStyle(Theme.strong)
                                .padding(14)
                                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
                            }
                        }
                        FieldLabel(text: "Or enter the Mac's address").padding(.top, 8)
                        HStack {
                            FormField(placeholder: "192.168.1.10:47640", text: $address, monospaced: true)
                                .keyboardType(.numbersAndPunctuation)
                                .textInputAutocapitalization(.never)
                            Button("Next") { if let mac = MacSync.endpoint(fromAddress: address) { choose(mac) } }
                                .buttonStyle(SecondaryButtonStyle())
                                .disabled(MacSync.endpoint(fromAddress: address) == nil)
                        }
                    }
                }
                .padding(20)
            }
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .task {
            macs = await MacSync.discover()
            searching = false
            if macs.count == 1 { choose(macs[0]) }
        }
        .formSheet()
    }

    private func choose(_ mac: MacEndpoint) {
        chosen = mac
        codeFocused = true
    }
}
