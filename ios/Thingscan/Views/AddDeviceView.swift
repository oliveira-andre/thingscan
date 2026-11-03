import SwiftUI

/// Scans while open: OCF over the Wi-Fi, Bonjour and Bluetooth, plus devices in the
/// SmartThings account that weren't found nearby.
struct AddDeviceView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var found: [ScannedDevice] = []
    @State private var channels: [ScanChannel: ChannelState] = [:]
    @State private var adding: ScannedDevice?
    @State private var addingCloud: CloudDevice?
    @State private var scan: Task<Void, Never>?
    @State private var started = Date()

    private var running: Bool { channels.values.contains(.scanning) }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 24) {
                    scanPanel
                    let ready = merged.filter { $0.kind == .readyToAdd }
                    let nearby = merged.filter { $0.kind != .readyToAdd }
                    if !ready.isEmpty { section("Ready to add", "Waiting to be set up") { ForEach(ready) { row($0) } } }
                    if !nearby.isEmpty { section("Found nearby", "On your Wi-Fi or in Bluetooth range") { ForEach(nearby) { row($0) } } }
                    if !cloudOnly.isEmpty {
                        section("In your SmartThings account", "Not seen nearby, controlled through SmartThings") {
                            ForEach(cloudOnly) { cloudRow($0) }
                        }
                    }
                    if merged.isEmpty && cloudOnly.isEmpty && !running {
                        Text("Nothing found nearby. Make sure the device is on and on the same Wi-Fi, or put it in pairing mode so it advertises over Bluetooth.")
                            .font(.system(size: 14)).foregroundStyle(Theme.muted).padding(.top, 20)
                    }
                }
                .padding(16)
            }
            .background(Theme.canvas)
            .navigationTitle("Add device")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .primaryAction) {
                    Button { start() } label: { Image(systemName: "arrow.clockwise") }.disabled(running)
                }
            }
            .sheet(item: $adding) { device in
                AddSheet(title: device.name, subtitle: Format.subtitle(manufacturer: device.manufacturer, model: device.model, kind: device.kind, details: device.details),
                         kind: device.kind, details: device.details, note: note(for: device)) { name, room in
                    let saved = model.add(device, name: name, room: room)
                    finish(saved)
                }
            }
            .sheet(item: $addingCloud) { cloud in
                AddSheet(title: cloud.name, subtitle: ["SmartThings", Format.category(cloud.category), cloud.model].compactMap { $0 }.joined(separator: " · "),
                         kind: AppModel.kind(ofCategory: cloud.category), details: ["cloud.category": cloud.category ?? ""],
                         note: "Controlled through your SmartThings account, which also tells Thingscan whether it's online.") { name, room in
                    let saved = model.addCloud(cloud, name: name, room: room)
                    finish(saved)
                }
            }
        }
        .onAppear(perform: start)
        .onDisappear { scan?.cancel() }
    }

    private func start() {
        found = []
        started = Date()
        channels = Dictionary(uniqueKeysWithValues: ScanChannel.allCases.map { ($0, .scanning) })
        scan = Task {
            await Scanner.run { channel, state, devices in
                channels[channel] = state
                found.removeAll { device in devices.contains { $0.key == device.key } }
                found += devices
            }
        }
    }

    private func finish(_ saved: SavedDevice) {
        adding = nil
        addingCloud = nil
        dismiss()
        Task { await model.checkAll(ids: [saved.id]) }
    }

    /// LAN findings for the same address are one device (an OCF TV that is also on Bonjour).
    private var merged: [ScannedDevice] {
        var out: [ScannedDevice] = []
        for device in found {
            if !device.address.isEmpty, device.sources != ["ble"], let index = out.firstIndex(where: { $0.address == device.address && $0.sources != ["ble"] }) {
                out[index].ids += device.ids.filter { !out[index].ids.contains($0) }
                out[index].endpoints += device.endpoints.filter { !out[index].endpoints.contains($0) }
                out[index].sources += device.sources
            } else {
                out.append(device)
            }
        }
        return out.sorted { ($0.kind.rawValue, $0.name) < ($1.kind.rawValue, $1.name) }
    }

    private var cloudOnly: [CloudDevice] {
        guard model.hasCloudToken, model.cloudError == nil else { return [] }
        let linked = Set(model.devices.compactMap(\.cloudId))
        return model.cloudDevices.filter { cloud in
            !linked.contains(cloud.deviceId) && !(cloud.di.map { di in found.contains { $0.ids.contains("ocf:\(di)") } } ?? false)
        }
    }

    private func added(_ device: ScannedDevice) -> Bool { model.devices.contains { $0.sharesIdentity(with: device.ids) } }

    private func note(for device: ScannedDevice) -> String {
        let ways = Set(device.endpoints.compactMap { e -> String? in if case .softAp = e { nil } else { e.label } })
        let checks = ways.isEmpty ? "It can't be checked automatically." : "Each time Thingscan opens it checks this device over \(ways.sorted().joined(separator: ", "))."
        return device.kind == .readyToAdd
            ? checks + " It's waiting to be set up; joining it to your Wi-Fi still happens in its maker's app."
            : checks
    }

    // MARK: Pieces

    private var scanPanel: some View {
        VStack(alignment: .leading, spacing: 14) {
            TimelineView(.periodic(from: .now, by: 0.2)) { context in
                ProgressView(value: running ? min(1, context.date.timeIntervalSince(started) / 7) : 1)
                    .tint(running ? Theme.primary : Theme.raised)
            }
            FlowLayout(spacing: 8) {
                ForEach(ScanChannel.allCases) { channel in
                    let state = channels[channel] ?? .scanning
                    HStack(spacing: 6) {
                        switch state {
                        case .scanning: ProgressView().controlSize(.mini).tint(Theme.primary)
                        case .done: Image(systemName: "checkmark").foregroundStyle(Theme.up)
                        case .failed: Image(systemName: "exclamationmark.circle").foregroundStyle(Theme.pending)
                        }
                        Text(channel.label)
                        if case let .done(count) = state { Text("\(count)").font(Theme.number(13)).foregroundStyle(Theme.primary) }
                    }
                    .font(.system(size: 13, weight: .medium))
                    .foregroundStyle(state == .scanning ? Theme.mutedStrong : Theme.strong)
                    .padding(.horizontal, 12)
                    .frame(height: 32)
                    .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
                }
            }
            if case let .failed(reason) = channels[.ble] {
                Text(reason).font(.system(size: 12)).foregroundStyle(Theme.pending)
            }
        }
        .card(padding: 18)
    }

    private func section<Rows: View>(_ title: String, _ hint: String, @ViewBuilder rows: () -> Rows) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(title).font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong)
                Text(hint).font(.system(size: 12)).foregroundStyle(Theme.muted)
            }
            VStack(spacing: 0) { rows() }
                .padding(.horizontal, 16)
                .background(RoundedRectangle(cornerRadius: 12).fill(Theme.card))
        }
    }

    private func row(_ device: ScannedDevice) -> some View {
        HStack(spacing: 12) {
            KindTile(kind: device.kind, name: device.name, details: device.details, size: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(device.name).font(.system(size: 15, weight: .semibold)).foregroundStyle(Theme.strong).lineLimit(1)
                Text(Format.subtitle(manufacturer: device.manufacturer, model: device.model, kind: device.kind, details: device.details))
                    .font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
                Text(device.address).font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.muted).lineLimit(1)
            }
            Spacer()
            if added(device) {
                Label("Added", systemImage: "checkmark").font(.system(size: 13, weight: .medium)).foregroundStyle(Theme.mutedStrong)
            } else {
                Button { adding = device } label: { Label("Add", systemImage: "plus") }.buttonStyle(PrimaryButtonStyle(compact: true))
            }
        }
        .padding(.vertical, 12)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.hairline).frame(height: 1) }
    }

    private func cloudRow(_ cloud: CloudDevice) -> some View {
        HStack(spacing: 12) {
            KindTile(kind: AppModel.kind(ofCategory: cloud.category), name: cloud.name, details: ["cloud.category": cloud.category ?? ""], size: 36)
            VStack(alignment: .leading, spacing: 2) {
                Text(cloud.name).font(.system(size: 15, weight: .semibold)).foregroundStyle(Theme.strong).lineLimit(1)
                Text([Format.category(cloud.category), cloud.model].compactMap { $0 }.joined(separator: " · "))
                    .font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
            }
            Spacer()
            Button { addingCloud = cloud } label: { Label("Add", systemImage: "plus") }.buttonStyle(PrimaryButtonStyle(compact: true))
        }
        .padding(.vertical, 12)
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.hairline).frame(height: 1) }
    }
}

/// Name and room for a device being added.
struct AddSheet: View {
    @Environment(\.dismiss) private var dismiss
    let title: String
    let subtitle: String
    let kind: Kind
    let details: [String: String]
    let note: String
    let save: (String, String?) -> Void
    @State private var name = ""
    @State private var room = ""

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 12) {
                    KindTile(kind: kind, name: title, details: details)
                    VStack(alignment: .leading) {
                        Text("Add device").font(.system(size: 20, weight: .semibold)).foregroundStyle(Theme.strong)
                        Text(subtitle).font(.system(size: 13)).foregroundStyle(Theme.muted)
                    }
                }
                FieldLabel(text: "Name")
                FormField(placeholder: "Name", text: $name)
                FieldLabel(text: "Room")
                FormField(placeholder: "e.g. Living room", text: $room)
                RoomSuggestions(room: $room)
                Text(note).font(.system(size: 13)).foregroundStyle(Theme.muted)
                Spacer()
                Button { save(name, room.isEmpty ? nil : room) } label: { Label("Add", systemImage: "plus").frame(maxWidth: .infinity) }
                    .buttonStyle(PrimaryButtonStyle())
            }
            .padding(20)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .onAppear { name = title }
        .presentationDetents([.large])
        .formSheet()
    }
}
