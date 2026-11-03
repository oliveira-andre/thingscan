import SwiftUI
import UIKit.UIGestureRecognizerSubclass

/// One device: the TV remote for a TV, the appliance controls for anything else, and Details.
struct DeviceView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let id: String
    @State private var tab = Tab.control
    @State private var editing = false

    enum Tab: Hashable { case control, details }

    var body: some View {
        if let device = model.device(id) {
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    header(device)
                    TabBar(tabs: [(Tab.control, device.isTV ? "Remote" : "Controls"), (Tab.details, "Details")], selection: $tab)
                    switch tab {
                    case .control:
                        if device.isTV { RemoteView(device: device) } else { ControlsView(device: device) }
                    case .details:
                        DetailsView(device: device)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 32)
            }
            .background(Theme.canvas)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { editing = true } label: { Image(systemName: "pencil") }
                        .accessibilityLabel("Edit name and room")
                }
            }
            .sheet(isPresented: $editing) { EditDeviceSheet(device: device) }
        } else {
            Color.clear.onAppear { dismiss() }
        }
    }

    private func header(_ device: SavedDevice) -> some View {
        let state = status(of: model.checks[device.id], device: device)
        return HStack(spacing: 12) {
            KindTile(kind: device.kind, name: device.name, details: device.details, size: 48)
            VStack(alignment: .leading, spacing: 3) {
                Text(device.name).font(.system(size: 22, weight: .semibold)).foregroundStyle(Theme.strong).lineLimit(1)
                HStack(spacing: 6) {
                    Image(systemName: "mappin.and.ellipse").font(.system(size: 11))
                    Text(device.room ?? "No room")
                }
                .font(.system(size: 13)).foregroundStyle(Theme.muted)
                HStack(spacing: 6) {
                    StatusDot(tone: state.tone)
                    Text(state.text).font(Theme.number(13, .medium)).foregroundStyle(state.tone.color).lineLimit(1)
                }
            }
        }
        .padding(.top, 4)
    }
}

// MARK: Details

struct DetailsView: View {
    @Environment(AppModel.self) private var model
    let device: SavedDevice
    @State private var confirmRemove = false

    var body: some View {
        let check = model.checks[device.id]
        let state = status(of: check, device: device)
        VStack(alignment: .leading, spacing: 16) {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    StatusDot(tone: state.tone)
                    Text(state.text).font(Theme.number(14, .medium)).foregroundStyle(state.tone.color)
                    Spacer()
                    Button {
                        Task { await model.checkAll(ids: [device.id]) }
                    } label: {
                        Label("Check now", systemImage: "arrow.clockwise")
                    }
                    .buttonStyle(SecondaryButtonStyle(compact: true))
                    .disabled(check == .checking)
                }
                if case let .done(report) = check {
                    ForEach(report.probes, id: \.self) { probe in
                        Divider().overlay(Theme.hairline)
                        HStack(alignment: .top, spacing: 12) {
                            Chip(label: probe.via == "SmartThings" ? "API" : probe.via, status: probe.status).frame(width: 96, alignment: .leading)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(probe.target).font(.system(size: 12, design: .monospaced)).foregroundStyle(Theme.text).lineLimit(1)
                                Text(probe.detail).font(.system(size: 12)).foregroundStyle(Theme.muted)
                            }
                            Spacer()
                            if let ms = probe.latencyMs { Text("\(ms) ms").font(Theme.number(13)).foregroundStyle(Theme.mutedStrong) }
                        }
                    }
                }
            }
            .card(padding: 16)

            VStack(alignment: .leading, spacing: 10) {
                fact("Room", device.room ?? "None")
                fact("Kind", device.kind.label)
                if let maker = device.manufacturer { fact("Maker", maker) }
                if let model = device.model { fact("Model", model) }
                if let address = device.lastAddress { fact("Address", address) }
                fact("Last seen", device.lastSeen.map { Format.ago($0) } ?? "not since it was added")
                fact("Added", Date(timeIntervalSince1970: TimeInterval(device.addedAt)).formatted(date: .abbreviated, time: .omitted))
                if device.syncedFromMac { fact("From", "Thingscan on your Mac") }
            }

            DisclosureGroup("Identity") {
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(device.ids, id: \.self) { id in
                        Text(id).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.text).textSelection(.enabled)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 8)
            }
            .font(.system(size: 14, weight: .semibold))
            .tint(Theme.mutedStrong)

            Button(role: .destructive) { confirmRemove = true } label: { Label("Remove from home", systemImage: "trash") }
                .buttonStyle(SecondaryButtonStyle(destructive: true))
                .padding(.top, 8)
                .confirmationDialog("Remove \(device.name)?", isPresented: $confirmRemove, titleVisibility: .visible) {
                    Button("Remove", role: .destructive) { model.remove(device.id) }
                } message: {
                    Text("Thingscan stops checking it on this phone. You can add it again from a scan.")
                }
        }
    }

    private func fact(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(label).foregroundStyle(Theme.muted).frame(width: 96, alignment: .leading)
            Text(value).foregroundStyle(Theme.text).textSelection(.enabled)
            Spacer()
        }
        .font(.system(size: 14))
    }
}

// MARK: TV remote

struct RemoteView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    let device: SavedDevice
    @State private var remote: TVRemote?
    @State private var phase = Phase.connecting
    @State private var message = ""
    /// Counts connection attempts, so one left over from before the phone was locked can tell
    /// it's been replaced.
    @State private var attempt = 0
    /// Connected or connecting when the app went to the background; reconnects on the way back.
    @State private var suspended = false
    @AppStorage("remoteMode") private var mode = Mode.buttons
    @State private var showNumbers = false

    enum Phase { case connecting, allow, ready, off, waking, error }
    enum Mode: String { case buttons, touchpad }

    private static let apps = [("Netflix", "3201907018807"), ("YouTube", "111299001912"), ("Prime Video", "3201910019365"), ("Disney+", "3201901017640")]

    var body: some View {
        VStack(spacing: 16) {
            statusBanner
            // Outside the remote so the mode can be chosen while the TV is still connecting.
            Picker("Mode", selection: $mode) {
                Text("Buttons").tag(Mode.buttons)
                Text("Touchpad").tag(Mode.touchpad)
            }
            .pickerStyle(.segmented)
            .frame(width: 220)
            VStack(spacing: 18) {
                HStack(spacing: 14) {
                    powerKey
                    key("KEY_SOURCE", symbol: "rectangle.on.rectangle", label: "Source")
                    key("KEY_MUTE", symbol: "speaker.slash", label: "Mute")
                }
                HStack(alignment: .center, spacing: 12) {
                    rocker("VOL", up: "KEY_VOLUP", down: "KEY_VOLDOWN")
                    Group {
                        if mode == .buttons { dpad } else { TouchpadView(send: pointer, click: pointerClick, scroll: { send($0 ? "KEY_CHDOWN" : "KEY_CHUP") }, select: { send("KEY_ENTER") }) }
                    }
                    .frame(maxWidth: .infinity)
                    rocker("CH", up: "KEY_CHUP", down: "KEY_CHDOWN")
                }
                HStack(spacing: 14) {
                    key("KEY_RETURN", symbol: "arrow.uturn.backward", label: "Back")
                    key("KEY_HOME", symbol: "house", label: "Home")
                    key("KEY_MENU", symbol: "line.3.horizontal", label: "Menu")
                }
                HStack(spacing: 14) {
                    key("KEY_REWIND", symbol: "backward.fill", label: "Rewind")
                    key("KEY_PLAY", symbol: "play.fill", label: "Play")
                    key("KEY_PAUSE", symbol: "pause.fill", label: "Pause")
                    key("KEY_FF", symbol: "forward.fill", label: "Fast forward")
                }
                FlowLayout(spacing: 8) {
                    ForEach(Self.apps, id: \.1) { app in
                        Button(app.0) { launch(app.1) }
                            .font(.system(size: 13, weight: .semibold))
                            .padding(.horizontal, 12)
                            .frame(height: 32)
                            .foregroundStyle(Theme.text)
                            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Theme.hairline))
                    }
                }
                DisclosureGroup("Number pad", isExpanded: $showNumbers) {
                    LazyVGrid(columns: Array(repeating: GridItem(.fixed(64), spacing: 12), count: 3), spacing: 12) {
                        ForEach(1 ... 9, id: \.self) { n in textKey("KEY_\(n)", "\(n)") }
                        key("KEY_CH_LIST", symbol: "list.bullet", label: "Channel list")
                        textKey("KEY_0", "0")
                        textKey("KEY_PRECH", "Prev")
                    }
                    .padding(.top, 12)
                }
                .font(.system(size: 14, weight: .semibold))
                .tint(Theme.mutedStrong)
            }
            .padding(.vertical, 22)
            .padding(.horizontal, 14)
            .background(RoundedRectangle(cornerRadius: 12).fill(Theme.card))
            .opacity(phase == .ready ? 1 : 0.4)
            .allowsHitTesting(phase == .ready)
        }
        .task { await connect() }
        .onChange(of: scenePhase) { _, now in
            switch now {
            case .background where [.ready, .connecting, .allow].contains(phase):
                // iOS cuts the TV's socket while the phone is locked or in another app, and the
                // first press afterwards would only find that out. Let it go now, and open a
                // fresh one on the way back, alongside the device check.
                suspended = true
                attempt += 1
                remote?.close()
                remote = nil
                phase = .connecting
            case .active where suspended:
                suspended = false
                Task { await connect() }
            default:
                break
            }
        }
        .onDisappear {
            attempt += 1
            remote?.close()
            remote = nil
        }
    }

    // MARK: Connection

    private var tvIP: String? {
        for endpoint in device.endpoints {
            switch endpoint {
            case let .samsungTv(ip, _, _), let .ocf(ip, _): return ip
            case let .upnp(location, _): if let host = URL(string: location)?.host { return host }
            default: continue
            }
        }
        return device.lastAddress?.split(separator: ":").first.map(String.init)
    }

    /// Learned from the TV's API while it's on, here or on the Mac.
    private var tvMac: String? {
        for endpoint in device.endpoints {
            if case let .samsungTv(_, _, mac?) = endpoint { return mac }
        }
        return device.details["tv.wifi_mac"]
    }

    private func connect() async {
        guard let ip = tvIP else {
            phase = .error
            message = "No network address is known for this TV."
            return
        }
        attempt += 1
        let mine = attempt
        phase = device.tvToken == nil ? .allow : .connecting
        let remote = TVRemote(ip: ip)
        do {
            var token: String?
            do {
                token = try await remote.connect(name: "Thingscan", token: device.tvToken, wait: device.tvToken == nil ? 30 : 8)
            } catch TVError.noAnswer where device.tvToken != nil && attempt == mine {
                // The TV didn't take the saved pairing and is asking to Allow this iPhone: wait for that.
                phase = .allow
                token = try await remote.connect(name: "Thingscan", token: nil, wait: 30)
            }
            if let token { model.setTVToken(device.id, token) }
            guard attempt == mine else { return remote.close() }
            self.remote?.close()
            self.remote = remote
            phase = .ready
            if tvMac == nil, let mac = await SamsungTV.info(ip: ip)?.mac { model.learnTVMac(device.id, mac: mac) }
        } catch {
            guard attempt == mine else { return }
            message = error.localizedDescription
            phase = (error as? TVError) == .unreachable ? .off : .error
        }
    }

    private func perform(_ action: @escaping (TVRemote) async throws -> Void) {
        Task { await run(action) }
    }

    /// Runs `action` on the remote, reconnecting once if the socket dropped. False when it still failed.
    @discardableResult
    private func run(_ action: (TVRemote) async throws -> Void) async -> Bool {
        guard let remote else { return false }
        do {
            try await action(remote)
            return true
        } catch {
            // The socket may have dropped; reconnect once and try again.
            await connect()
            guard let fresh = self.remote else { return false }
            return (try? await action(fresh)) != nil
        }
    }

    private func send(_ key: String) {
        Haptics.tap()
        perform { try await $0.click(key) }
    }

    /// Sends `key` again and again while it's held, as a real remote repeats volume, channel and arrows.
    private func repeating(_ key: String) async {
        while !Task.isCancelled, await run({ try await $0.click(key) }) {
            Haptics.tap()
            try? await Task.sleep(nanoseconds: 150_000_000)
        }
    }

    /// Held, power goes down on the TV too and is pressed again every 0.12 s, as a real remote
    /// repeats a held key, until it's let go; a few seconds of that switches a Frame TV fully off.
    private func holdPower() async {
        Haptics.press()
        while !Task.isCancelled, await run({ try await $0.press("KEY_POWER") }) {
            try? await Task.sleep(nanoseconds: 120_000_000)
        }
        // This task is cancelled by now, so let go from a fresh one.
        let remote = remote
        Task { try? await remote?.release("KEY_POWER") }
    }

    private func launch(_ app: String) {
        Haptics.tap()
        perform { try await $0.launch(appId: app) }
    }

    private func pointer(_ dx: Int, _ dy: Int) { perform { try await $0.movePointer(dx: dx, dy: dy) } }

    private func pointerClick() {
        Haptics.press()
        perform { try await $0.pointerClick() }
    }

    // MARK: Pieces

    private var statusBanner: some View {
        HStack(spacing: 8) {
            switch phase {
            case .connecting:
                ProgressView().controlSize(.small).tint(Theme.primary)
                Text("Connecting to \(device.name)…").foregroundStyle(Theme.mutedStrong)
            case .allow:
                ProgressView().controlSize(.small).tint(Theme.primary)
                Text("Look at the TV and choose **Allow** so Thingscan can control it.").foregroundStyle(Theme.mutedStrong)
            case .ready:
                Circle().fill(Theme.up).frame(width: 8, height: 8)
                Text("Connected to \(device.name)").foregroundStyle(Theme.up)
            case .waking:
                ProgressView().controlSize(.small).tint(Theme.primary)
                Text(message).foregroundStyle(Theme.mutedStrong)
            case .off, .error:
                Image(systemName: "exclamationmark.circle").foregroundStyle(Theme.down)
                Text(message).foregroundStyle(Theme.down)
                Spacer()
                if phase == .off {
                    Button("Turn on") { Task { await turnOn() } }.buttonStyle(PrimaryButtonStyle(compact: true))
                }
                Button("Retry") { Task { await connect() } }.buttonStyle(SecondaryButtonStyle(compact: true))
            }
        }
        .font(.system(size: 14))
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
        .background(RoundedRectangle(cornerRadius: 8).fill(Theme.card))
    }

    /// Every way this phone has of switching the TV on, all at once: Wake-on-LAN sent to the TV
    /// itself (iOS won't let this app broadcast), the paired Mac sending it (it can), and
    /// SmartThings. Then waits for the TV to answer.
    private func turnOn() async {
        guard let ip = tvIP else { return }
        let mac = tvMac
        let pairing = model.pairing
        let cloud = device.cloudId.flatMap { id in model.cloud.map { ($0, id) } }
        guard mac != nil || cloud != nil else {
            message = "This TV's MAC address isn't known yet, and it isn't linked to SmartThings. Turn it on by hand once and open the remote; after that Thingscan can wake it."
            return
        }
        Haptics.press()
        phase = .waking
        message = "Turning on \(device.name)…"
        let attempts = await withTaskGroup(of: (String, Error?).self) { group in
            if let mac {
                group.addTask {
                    do { try await WakeOnLAN.send(mac: mac, ip: ip); return ("Wi-Fi", nil) } catch { return ("Wi-Fi", error) }
                }
                if let pairing {
                    group.addTask {
                        do { try await MacSync.wake(pairing, mac: mac); return ("your Mac", nil) } catch { return ("your Mac", error) }
                    }
                }
            }
            if let cloud {
                let (client, cloudId) = cloud
                group.addTask {
                    do {
                        try await client.command(cloudId, capability: "switch", command: "on")
                        return ("SmartThings", nil)
                    } catch {
                        return ("SmartThings", error)
                    }
                }
            }
            var all: [(String, Error?)] = []
            for await attempt in group { all.append(attempt) }
            return all
        }
        for case let (_, error?) in attempts { model.noteCloudError(error) }
        // The TV's API answers a few seconds after it wakes. Some answer in standby too, and
        // their remote's power key then does the rest.
        var answered = false
        for _ in 0 ..< 8 {
            try? await Task.sleep(nanoseconds: 2_500_000_000)
            guard let info = await SamsungTV.info(ip: ip, timeout: 1.5) else { continue }
            answered = true
            if info.power == nil || info.power == "on" { break }
        }
        if answered {
            await connect()
            if phase == .ready { return }
        }
        let failures = attempts.compactMap { via, error in error.map { "\(via): \($0.localizedDescription)" } }
        message = "The TV didn't wake up. Turn on “Power on with mobile” in its settings, or switch it on by hand."
            + (failures.isEmpty ? "" : " (" + failures.joined(separator: "; ") + ")")
        phase = .off
    }

    /// A tap toggles (Art mode on a Frame TV); holding it switches a Frame TV fully off.
    private var powerKey: some View {
        HoldKey(delay: 0.6, tap: { send("KEY_POWER") }, hold: { await holdPower() }) {
            Image(systemName: "power")
                .font(.system(size: 18, weight: .semibold))
                .foregroundStyle(Theme.strong)
                .frame(width: 60, height: 46)
                .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
        }
        .accessibilityLabel("Power")
        .accessibilityHint("Hold to switch a Frame TV fully off")
    }

    private func repeatingKey(_ code: String, @ViewBuilder label: () -> some View) -> some View {
        HoldKey(tap: { send(code) }, hold: { await repeating(code) }, label: label)
    }

    private func key(_ code: String, symbol: String, label: String) -> some View {
        Button { send(code) } label: {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(Theme.strong)
                .frame(width: 60, height: 46)
                .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
        }
        .buttonStyle(PressableStyle())
        .accessibilityLabel(label)
    }

    private func textKey(_ code: String, _ text: String) -> some View {
        Button { send(code) } label: {
            Text(text)
                .font(Theme.number(17))
                .foregroundStyle(Theme.strong)
                .frame(width: 64, height: 46)
                .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
        }
        .buttonStyle(PressableStyle())
    }

    /// Volume on the left, channels on the right: an up arrow, the label, a down arrow.
    private func rocker(_ label: String, up: String, down: String) -> some View {
        VStack(spacing: 6) {
            repeatingKey(up) {
                Image(systemName: "chevron.up").font(.system(size: 18, weight: .bold)).frame(width: 52, height: 54)
            }
            .accessibilityLabel("\(label == "VOL" ? "Volume" : "Channel") up")
            Text(label).font(.system(size: 11, weight: .semibold)).foregroundStyle(Theme.muted)
            repeatingKey(down) {
                Image(systemName: "chevron.down").font(.system(size: 18, weight: .bold)).frame(width: 52, height: 54)
            }
            .accessibilityLabel("\(label == "VOL" ? "Volume" : "Channel") down")
        }
        .foregroundStyle(Theme.strong)
        .padding(.vertical, 6)
        .background(Capsule().fill(Theme.raised))
    }

    private var dpad: some View {
        ZStack {
            Circle().fill(Theme.raised).frame(width: 176, height: 176)
            VStack(spacing: 0) {
                arrow("KEY_UP", "chevron.up", "Up")
                HStack(spacing: 0) {
                    arrow("KEY_LEFT", "chevron.left", "Left")
                    Button { send("KEY_ENTER") } label: {
                        Text("OK").font(.system(size: 16, weight: .bold)).foregroundStyle(Theme.onPrimary)
                            .frame(width: 60, height: 60).background(Circle().fill(Theme.primary))
                    }
                    .buttonStyle(PressableStyle())
                    arrow("KEY_RIGHT", "chevron.right", "Right")
                }
                arrow("KEY_DOWN", "chevron.down", "Down")
            }
        }
    }

    private func arrow(_ code: String, _ symbol: String, _ label: String) -> some View {
        repeatingKey(code) {
            Image(systemName: symbol).font(.system(size: 18, weight: .bold)).foregroundStyle(Theme.strong).frame(width: 56, height: 56)
        }
        .accessibilityLabel(label)
    }
}

/// A trackpad for the TV's pointer: drag to move it (sent at most every 40 ms), tap to click.
/// With a second finger down, sliding scrolls: each `scrollStep` is a channel up or down, which
/// scroll the TV's browser. The OK button below sends the remote's Enter key, which every app
/// understands.
struct TouchpadView: View {
    let send: (Int, Int) -> Void
    let click: () -> Void
    /// True to scroll down: sliding up, as content follows the finger on a touch screen.
    let scroll: (_ down: Bool) -> Void
    let select: () -> Void
    @State private var pendingX: Double = 0
    @State private var pendingY: Double = 0
    @State private var pendingScroll: Double = 0
    @State private var lastSent = Date.distantPast
    @State private var touching = false
    private let speed = 2.0
    private let scrollStep = 50.0

    var body: some View {
        VStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 12)
                .fill(touching ? Color(hex: 0x363C45) : Theme.raised)
                .overlay(RoundedRectangle(cornerRadius: 12).stroke(Theme.hairline))
                .overlay {
                    if !touching {
                        VStack(spacing: 4) {
                            Image(systemName: "hand.point.up.left").font(.system(size: 20))
                            Text("Drag to move the pointer\nTap to click\nSlide a second finger to scroll")
                                .font(.system(size: 12)).multilineTextAlignment(.center)
                        }
                        .foregroundStyle(Theme.muted)
                    }
                }
                .overlay {
                    TouchSurface(
                        began: {
                            touching = true
                            pendingScroll = 0
                        },
                        moved: { delta in
                            pendingX += delta.x * speed
                            pendingY += delta.y * speed
                            flush()
                        },
                        scrolled: scrolled,
                        ended: { tapped in
                            touching = false
                            if tapped { click() } else { flush(force: true) }
                        }
                    )
                }
                .frame(height: 176)
                .accessibilityLabel("Touchpad")
            Button(action: select) {
                Text("OK").font(.system(size: 15, weight: .bold)).foregroundStyle(Theme.onPrimary)
                    .frame(width: 72, height: 36).background(Capsule().fill(Theme.primary))
            }
            .buttonStyle(PressableStyle())
        }
    }

    private func flush(force: Bool = false) {
        guard force || Date().timeIntervalSince(lastSent) > 0.04 else { return }
        let dx = Int(pendingX.rounded())
        let dy = Int(pendingY.rounded())
        guard dx != 0 || dy != 0 else { return }
        pendingX -= Double(dx)
        pendingY -= Double(dy)
        lastSent = Date()
        send(max(-200, min(200, dx)), max(-200, min(200, dy)))
    }

    private func scrolled(_ dy: CGFloat) {
        pendingScroll += dy
        while abs(pendingScroll) >= scrollStep {
            let down = pendingScroll < 0
            pendingScroll += down ? scrollStep : -scrollStep
            scroll(down)
        }
    }
}

/// The touchpad's touches. SwiftUI's drag gesture follows only one finger, so this is UIKit:
/// one finger moves, and once a second one is down the rest of the touch scrolls.
private struct TouchSurface: UIViewRepresentable {
    let began: () -> Void
    let moved: (CGPoint) -> Void
    let scrolled: (CGFloat) -> Void
    /// Whether it was a tap: one finger that barely moved.
    let ended: (_ tapped: Bool) -> Void

    func makeUIView(context: Context) -> UIView {
        let view = UIView()
        view.isMultipleTouchEnabled = true
        view.addGestureRecognizer(TouchpadRecognizer())
        return view
    }

    func updateUIView(_ view: UIView, context: Context) {
        guard let recognizer = view.gestureRecognizers?.first as? TouchpadRecognizer else { return }
        recognizer.surface = self
    }
}

private final class TouchpadRecognizer: UIGestureRecognizer {
    var surface: TouchSurface?
    private var touching = Set<UITouch>()
    private var scrolling = false
    private var travelled: CGFloat = 0

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        if touching.isEmpty {
            scrolling = false
            travelled = 0
            surface?.began()
        }
        touching.formUnion(touches)
        if touching.count > 1 { scrolling = true }
        // Starting straight away keeps the screen's scroll view from taking the touch.
        if state == .possible { state = .began }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        let deltas = touches.map { touch in
            let now = touch.location(in: view), before = touch.previousLocation(in: view)
            return CGPoint(x: now.x - before.x, y: now.y - before.y)
        }
        if scrolling {
            // One finger may rest while the other slides, so follow whichever moved most.
            if let dy = deltas.map(\.y).max(by: { abs($0) < abs($1) }) { surface?.scrolled(dy) }
        } else if let delta = deltas.first {
            travelled += abs(delta.x) + abs(delta.y)
            surface?.moved(delta)
        }
        state = .changed
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) { finish(touches, cancelled: false) }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) { finish(touches, cancelled: true) }

    private func finish(_ touches: Set<UITouch>, cancelled: Bool) {
        touching.subtract(touches)
        guard touching.isEmpty else { return }
        surface?.ended(!cancelled && !scrolling && travelled < 6)
        state = cancelled ? .cancelled : .ended
    }

    override func reset() {
        super.reset()
        touching.removeAll()
    }

    override func shouldBeRequiredToFail(by other: UIGestureRecognizer) -> Bool {
        other.view is UIScrollView
    }
}

struct PressableStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.94 : 1)
            .opacity(configuration.isPressed ? 0.8 : 1)
            .animation(.easeOut(duration: 0.08), value: configuration.isPressed)
    }
}

/// A remote key that does more while held: after `delay`, `hold` runs until the key is let go,
/// and the tap that would end the press is skipped. A quick tap, or VoiceOver's activate, calls `tap`.
struct HoldKey<Label: View>: View {
    let delay: Double
    let tap: () -> Void
    let hold: () async -> Void
    let label: Label
    @State private var holding: Task<Void, Never>?
    @State private var held = false

    init(delay: Double = 0.4, tap: @escaping () -> Void, hold: @escaping () async -> Void, @ViewBuilder label: () -> Label) {
        self.delay = delay
        self.tap = tap
        self.hold = hold
        self.label = label()
    }

    var body: some View {
        Button {
            if held { held = false } else { tap() }
        } label: { label }
        .buttonStyle(HoldStyle { pressed in
            holding?.cancel()
            holding = nil
            guard pressed else { return }
            held = false
            holding = Task {
                try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))
                guard !Task.isCancelled else { return }
                held = true
                await hold()
            }
        })
        .onDisappear { holding?.cancel() }
    }
}

/// PressableStyle that also says when the button goes down and comes back up.
private struct HoldStyle: ButtonStyle {
    let pressing: (Bool) -> Void

    func makeBody(configuration: Configuration) -> some View {
        PressableStyle().makeBody(configuration: configuration)
            .onChange(of: configuration.isPressed) { _, pressed in pressing(pressed) }
    }
}
