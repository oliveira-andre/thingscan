import SwiftUI

/// Every added device by room, with whether it answered the last check.
struct HomeView: View {
    @Environment(AppModel.self) private var model
    @State private var path: [String] = []
    @State private var showAdd = false
    @State private var showAccount = false
    @State private var editing: SavedDevice?
    @State private var renamingRoom: String?

    var body: some View {
        NavigationStack(path: $path) {
            ScrollView {
                VStack(alignment: .leading, spacing: 28) {
                    header
                    // A Mac that can't be reached isn't a problem here: it may just be closed. The
                    // account screen says so under the Mac.
                    if model.accountSession != nil, let error = model.accountError {
                        banner(error)
                    }
                    if model.devices.isEmpty {
                        empty
                    } else {
                        ForEach(groups, id: \.room) { group in
                            room(group.room, group.devices)
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 32)
            }
            .refreshable { await model.refreshAll() }
            .background(Theme.canvas)
            .toolbar {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    Button { showAccount = true } label: { accountIcon }
                        .accessibilityLabel("Account")
                    Button { showAdd = true } label: {
                        Image(systemName: "plus")
                            .font(.system(size: 15, weight: .bold))
                            .foregroundStyle(Theme.onPrimary)
                            .frame(width: 32, height: 32)
                            .background(Circle().fill(Theme.primary))
                    }
                    .accessibilityLabel("Add device")
                }
            }
            .toolbarBackground(Theme.canvas, for: .navigationBar)
            .navigationDestination(for: String.self) { id in DeviceView(id: id) }
            .sheet(isPresented: $showAdd) { AddDeviceView() }
            .sheet(isPresented: $showAccount) { AccountView() }
            .sheet(item: $editing) { device in EditDeviceSheet(device: device) }
            .sheet(item: Binding(get: { renamingRoom.map(RoomName.init) }, set: { renamingRoom = $0?.name })) { room in
                RenameRoomSheet(room: room.name)
            }
        }
    }

    private var groups: [(room: String, devices: [SavedDevice])] {
        let byRoom = Dictionary(grouping: model.devices) { $0.room ?? "" }
        return byRoom.keys.sorted { a, b in a.isEmpty ? false : b.isEmpty ? true : a < b }
            .map { ($0, byRoom[$0]!.sorted { $0.name < $1.name }) }
    }

    private var header: some View {
        let reachable = model.devices.filter { if case let .done(r) = model.checks[$0.id] { r.status == .reachable } else { false } }.count
        var parts = model.devices.isEmpty ? ["No devices yet"] : ["\(model.devices.count) device\(model.devices.count == 1 ? "" : "s")"]
        if !model.devices.isEmpty {
            parts.append(model.checking ? "checking…" : "\(reachable) reachable")
            if let last = model.lastCheck, !model.checking { parts.append("checked \(Format.clock(last))") }
        }
        return VStack(alignment: .leading, spacing: 6) {
            // In the content rather than the toolbar, where iOS 26 shrinks it to its icon.
            Label("Thingscan", systemImage: "dot.radiowaves.left.and.right")
                .font(.system(size: 15, weight: .bold))
                .foregroundStyle(Theme.primary)
                .padding(.bottom, 4)
            Text("Home").font(.system(size: 32, weight: .semibold)).foregroundStyle(Theme.strong)
            Text(parts.joined(separator: " · ")).font(Theme.number(14, .regular)).foregroundStyle(Theme.mutedStrong)
        }
        .padding(.top, 8)
    }

    private var accountIcon: some View {
        Image(systemName: "person.crop.circle")
            .font(.system(size: 22))
            .foregroundStyle(Theme.mutedStrong)
            .overlay(alignment: .bottomTrailing) {
                if model.hasCloudToken || model.pairing != nil || model.accountSession != nil {
                    Circle()
                        .fill(model.cloudError != nil || model.accountError != nil ? Theme.pending : Theme.up)
                        .frame(width: 9, height: 9)
                        .overlay(Circle().stroke(Theme.canvas, lineWidth: 2))
                }
            }
    }

    private func banner(_ text: String) -> some View {
        HStack(spacing: 8) {
            Image(systemName: "exclamationmark.circle")
            Text(text).font(.system(size: 13))
        }
        .foregroundStyle(Theme.pending)
        .card(padding: 14)
    }

    private var empty: some View {
        VStack(spacing: 14) {
            Image(systemName: "dot.radiowaves.left.and.right")
                .font(.system(size: 34))
                .foregroundStyle(Theme.primary)
                .frame(width: 88, height: 88)
                .background(Circle().fill(Theme.card))
            Text("No devices yet").font(.system(size: 20, weight: .semibold)).foregroundStyle(Theme.strong)
            Text("Add your TV and appliances from a scan, or sign in to your Thingscan account to bring over the devices from your Mac.")
                .font(.system(size: 14)).foregroundStyle(Theme.muted).multilineTextAlignment(.center)
            HStack(spacing: 8) {
                Button("Sign in") { showAccount = true }.buttonStyle(SecondaryButtonStyle())
                Button { showAdd = true } label: { Label("Add device", systemImage: "plus") }
                    .buttonStyle(PrimaryButtonStyle(pill: true))
            }
            .padding(.top, 6)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 48)
    }

    private func room(_ name: String, _ devices: [SavedDevice]) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            if groups.count > 1 || !name.isEmpty {
                HStack(spacing: 8) {
                    Image(systemName: name.isEmpty ? "house" : "mappin.and.ellipse").font(.system(size: 12)).foregroundStyle(Theme.muted)
                    Text(name.isEmpty ? "Other devices" : name).font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong)
                    if !name.isEmpty {
                        Button { renamingRoom = name } label: {
                            Image(systemName: "pencil").font(.system(size: 13)).foregroundStyle(Theme.muted)
                        }
                        .accessibilityLabel("Rename \(name)")
                    }
                }
            }
            LazyVGrid(columns: [GridItem(.flexible(), spacing: 12), GridItem(.flexible())], spacing: 12) {
                ForEach(devices) { device in card(device) }
            }
        }
    }

    private func card(_ device: SavedDevice) -> some View {
        let state = status(of: model.checks[device.id], device: device)
        return Button {
            path.append(device.id)
        } label: {
            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .top) {
                    KindTile(kind: device.kind, name: device.name, details: device.details)
                    Spacer()
                    StatusDot(tone: state.tone)
                }
                .padding(.bottom, 10)
                Text(device.name).font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong).lineLimit(1)
                Text(device.subtitle).font(.system(size: 12)).foregroundStyle(Theme.muted).lineLimit(1)
                Text(state.text).font(Theme.number(13, .medium)).foregroundStyle(state.tone.color).lineLimit(1).padding(.top, 8)
            }
            .card(padding: 16)
        }
        .buttonStyle(.plain)
        .contextMenu {
            Button { editing = device } label: { Label("Edit name and room", systemImage: "pencil") }
            Button(role: .destructive) { model.remove(device.id) } label: { Label("Remove from home", systemImage: "trash") }
        }
    }
}

private struct RoomName: Identifiable {
    var name: String
    var id: String { name }
}

/// Name and room for a saved device.
struct EditDeviceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let device: SavedDevice
    @State private var name = ""
    @State private var room = ""

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                HStack(spacing: 12) {
                    KindTile(kind: device.kind, name: device.name, details: device.details)
                    VStack(alignment: .leading) {
                        Text("Edit device").font(.system(size: 20, weight: .semibold)).foregroundStyle(Theme.strong)
                        Text(device.subtitle).font(.system(size: 13)).foregroundStyle(Theme.muted)
                    }
                }
                FieldLabel(text: "Name")
                FormField(placeholder: "Name", text: $name)
                FieldLabel(text: "Room")
                FormField(placeholder: "No room", text: $room)
                RoomSuggestions(room: $room)
                Spacer()
                Button {
                    model.update(device.id, name: name, room: room)
                    dismiss()
                } label: {
                    Text("Save").frame(maxWidth: .infinity)
                }
                .buttonStyle(PrimaryButtonStyle())
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            .padding(20)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .onAppear {
            name = device.name
            room = device.room ?? ""
        }
        .presentationDetents([.medium, .large])
        .formSheet()
    }
}

/// Existing rooms as one-tap choices.
struct RoomSuggestions: View {
    @Environment(AppModel.self) private var model
    @Binding var room: String

    var body: some View {
        if !model.rooms.isEmpty {
            FlowLayout(spacing: 6) {
                ForEach(model.rooms, id: \.self) { name in
                    Button(name) { room = name }
                        .font(.system(size: 13, weight: .medium))
                        .padding(.horizontal, 10)
                        .frame(height: 28)
                        .foregroundStyle(room == name ? Theme.primary : Theme.mutedStrong)
                        .background(RoundedRectangle(cornerRadius: 6).fill(room == name ? Theme.primary.opacity(0.15) : Theme.raised))
                }
            }
        }
    }
}

struct RenameRoomSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let room: String
    @State private var name = ""

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 16) {
                Text("Rename room").font(.system(size: 20, weight: .semibold)).foregroundStyle(Theme.strong)
                FieldLabel(text: "Room name")
                FormField(placeholder: "Room name", text: $name)
                Text("Leave it empty to take these devices out of any room. Using another room's name merges them.")
                    .font(.system(size: 13)).foregroundStyle(Theme.muted)
                Spacer()
                Button {
                    model.renameRoom(room, to: name)
                    dismiss()
                } label: {
                    Text("Save").frame(maxWidth: .infinity)
                }
                .buttonStyle(PrimaryButtonStyle())
            }
            .padding(20)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .onAppear { name = room }
        .presentationDetents([.medium])
        .formSheet()
    }
}
