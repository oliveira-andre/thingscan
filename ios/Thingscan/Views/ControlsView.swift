import SwiftUI

/// Appliance controls through the SmartThings account, built from the capabilities the device
/// reports: an air conditioner gets temperature, mode, fan and swing; a washer or dryer its
/// cycle, start/pause/stop and options.
struct ControlsView: View {
    @Environment(AppModel.self) private var model
    let device: SavedDevice
    @State private var status: JSONValue?
    @State private var failure: String?
    @State private var loading = false
    @State private var touched = Date.distantPast
    @State private var showConnect = false
    @State private var target: Double?
    @State private var targetTask: Task<Void, Never>?
    @State private var lightPending: (level: String, until: Date)?

    private static let operatingStates = ["washerOperatingState", "dryerOperatingState", "dishwasherOperatingState"]
    private static let handled = Set([
        "switch", "temperatureMeasurement", "relativeHumidityMeasurement", "thermostatCoolingSetpoint", "airConditionerMode",
        "airConditionerFanMode", "fanOscillationMode", "custom.airConditionerOptionalMode", "custom.thermostatSetpointControl",
        "remoteControlStatus", "powerConsumptionReport", "custom.washerSpinLevel", "custom.washerWaterTemperature",
        "custom.washerRinseCycles", "custom.washerSoilLevel", "samsungce.washerOperatingState", "custom.disabledCapabilities",
        "ocf", "execute", "refresh", lighting,
    ] + operatingStates)
    private static let lighting = "samsungce.airConditionerLighting"

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            content
        }
        .task(id: device.cloudId) {
            await load()
            // Keep the status fresh while it's on screen, but not right after the user changed something.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 10_000_000_000)
                if Date().timeIntervalSince(touched) > 5 { await load(quiet: true) }
            }
        }
        .sheet(isPresented: $showConnect) { ConnectSmartThingsSheet() }
    }

    @ViewBuilder private var content: some View {
        if !model.hasCloudToken {
            Callout(symbol: "icloud", title: "Control \(device.name) through SmartThings",
                    message: "Samsung appliances only take commands from the account that owns them, so Thingscan sends them through your SmartThings account, like the SmartThings app does.") {
                Button { showConnect = true } label: { Label("Connect SmartThings", systemImage: "link") }.buttonStyle(PrimaryButtonStyle())
            }
        } else if let error = model.cloudError {
            Callout(symbol: "key", title: model.cloudRenews ? "SmartThings needs attention" : "SmartThings needs a new token", message: error) {
                Button { showConnect = true } label: {
                    Label(model.cloudRenews ? "Connect again" : "Connect SmartThings", systemImage: "key")
                }
                .buttonStyle(PrimaryButtonStyle())
            }
        } else if device.cloudId == nil {
            linkPicker
        } else if let status {
            controls(status["components"]?["main"] ?? .null)
        } else if let failure {
            Callout(symbol: "exclamationmark.circle", title: "Couldn't load this device", message: failure) {
                Button("Try again") { Task { await load() } }.buttonStyle(SecondaryButtonStyle())
            }
        } else {
            HStack(spacing: 8) {
                ProgressView().tint(Theme.mutedStrong)
                Text("Loading from SmartThings…").foregroundStyle(Theme.muted)
            }
        }
    }

    private var linkPicker: some View {
        let linked = Set(model.devices.compactMap(\.cloudId))
        let candidates = model.cloudDevices.filter { !linked.contains($0.deviceId) }
        return Callout(symbol: "link", title: "Which SmartThings device is this?",
                       message: candidates.isEmpty ? "Your SmartThings account has no other devices." : "Thingscan couldn't match it to your SmartThings account by itself. Pick it once and it's remembered.") {
            VStack(alignment: .leading, spacing: 8) {
                ForEach(candidates) { cloud in
                    Button {
                        model.link(device.id, to: cloud.deviceId)
                    } label: {
                        HStack {
                            Text(cloud.name).foregroundStyle(Theme.strong)
                            Text(Format.category(cloud.category)).foregroundStyle(Theme.muted)
                            Spacer()
                            Image(systemName: "link").foregroundStyle(Theme.primary)
                        }
                        .font(.system(size: 14))
                        .padding(.horizontal, 12)
                        .frame(height: 40)
                        .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
                    }
                }
            }
        }
    }

    // MARK: Loading and commands

    private func load(quiet: Bool = false) async {
        guard let cloud = model.cloud, let cloudId = device.cloudId else { return }
        if !quiet { loading = true }
        defer { loading = false }
        do {
            status = try await cloud.status(cloudId)
            failure = nil
        } catch {
            model.noteCloudError(error)
            if status == nil { failure = error.localizedDescription }
        }
    }

    private func run(_ capability: String, _ command: String, _ arguments: [Any] = []) {
        guard let cloud = model.cloud, let cloudId = device.cloudId else { return }
        touched = Date()
        Haptics.tap()
        Task {
            do {
                try await cloud.command(cloudId, capability: capability, command: command, arguments: arguments)
                // SmartThings applies commands asynchronously; give the device a moment.
                try? await Task.sleep(nanoseconds: 1_200_000_000)
                await load(quiet: true)
            } catch {
                model.noteCloudError(error)
                failure = error.localizedDescription
            }
        }
    }

    // MARK: Controls

    @ViewBuilder
    private func controls(_ main: JSONValue) -> some View {
        let disabled = Set(value(main, "custom.disabledCapabilities", "disabledCapabilities")?.array?.compactMap(\.string) ?? [])
        let has = { (cap: String) in main[cap] != nil && !disabled.contains(cap) }
        hero(main, has)
        climate(main, has)
        laundry(main, has)
        energy(main, has)
        everything(main, disabled)
    }

    private func value(_ main: JSONValue, _ cap: String, _ attr: String) -> JSONValue? {
        guard let v = main[cap]?[attr]?["value"], !v.isNull else { return nil }
        return v
    }

    private func unit(_ main: JSONValue, _ cap: String, _ attr: String) -> String {
        main[cap]?[attr]?["unit"]?.string ?? "C"
    }

    @ViewBuilder
    private func hero(_ main: JSONValue, _ has: (String) -> Bool) -> some View {
        let power = has("switch") ? value(main, "switch", "switch")?.string : nil
        let temperature = has("temperatureMeasurement") ? value(main, "temperatureMeasurement", "temperature")?.double : nil
        let humidity = has("relativeHumidityMeasurement") ? value(main, "relativeHumidityMeasurement", "humidity")?.double : nil
        let operating = Self.operatingStates.first(where: has)
        HStack(alignment: .top) {
            HStack(alignment: .top, spacing: 22) {
                if let temperature {
                    reading("\(Int(temperature))°", "Room (°\(unit(main, "temperatureMeasurement", "temperature")))")
                }
                if let humidity { reading("\(Int(humidity))%", "Humidity", small: true) }
                if let operating { machineSummary(main, operating) }
            }
            Spacer()
            if let power {
                Button {
                    run("switch", power == "on" ? "off" : "on")
                } label: {
                    Label(power == "on" ? "On" : "Off", systemImage: "power")
                }
                .buttonStyle(PowerToggleStyle(on: power == "on"))
            }
        }
        .card(padding: 20)
    }

    private func reading(_ value: String, _ label: String, small: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(value).font(Theme.number(small ? 26 : 40, .bold)).foregroundStyle(Theme.strong).padding(.top, small ? 10 : 0)
            Text(label).font(.system(size: 12, weight: .medium)).foregroundStyle(Theme.muted)
        }
    }

    private func machineSummary(_ main: JSONValue, _ cap: String) -> some View {
        let machine = value(main, cap, "machineState")?.string ?? ""
        let job = value(main, cap, cap.replacingOccurrences(of: "OperatingState", with: "JobState"))?.string
            ?? value(main, "samsungce.washerOperatingState", "washerJobState")?.string
        let progress = value(main, "samsungce.washerOperatingState", "progress")?.double
        var parts: [String] = []
        if let job, job != "none" { parts.append(Format.value(job)) }
        if let left = minutesLeft(main, cap, machine) { parts.append("\(left) min left") }
        return VStack(alignment: .leading, spacing: 4) {
            Text(machine == "run" ? "Running" : machine == "pause" ? "Paused" : "Ready")
                .font(.system(size: 24, weight: .semibold)).foregroundStyle(Theme.strong).padding(.top, 8)
            Text(parts.isEmpty ? "Not running" : parts.joined(separator: " · ")).font(.system(size: 12)).foregroundStyle(Theme.muted)
            if machine == "run", let progress {
                ProgressView(value: progress, total: 100).tint(Theme.primary).frame(width: 130).padding(.top, 4)
            }
        }
    }

    private func minutesLeft(_ main: JSONValue, _ cap: String, _ machine: String) -> Int? {
        guard machine == "run" || machine == "pause" else { return nil }
        if let remaining = value(main, "samsungce.washerOperatingState", "remainingTime")?.double { return Int(remaining) }
        guard let text = value(main, cap, "completionTime")?.string, let end = ISO8601DateFormatter().date(from: text) else { return nil }
        let minutes = Int(end.timeIntervalSinceNow / 60)
        return minutes > 0 ? minutes : nil
    }

    @ViewBuilder
    private func climate(_ main: JSONValue, _ has: (String) -> Bool) -> some View {
        let modes: [(String, String, String, String, String)] = [
            ("airConditionerMode", "airConditionerMode", "supportedAcModes", "setAirConditionerMode", "Mode"),
            ("airConditionerFanMode", "fanMode", "supportedAcFanModes", "setFanMode", "Fan speed"),
            ("fanOscillationMode", "fanOscillationMode", "supportedFanOscillationModes", "setFanOscillationMode", "Swing"),
            ("custom.airConditionerOptionalMode", "acOptionalMode", "supportedAcOptionalMode", "setAcOptionalMode", "Special mode"),
        ]
        let choices = modes.compactMap { cap, attr, supported, command, label -> (String, String, String, String, [String], String)? in
            guard has(cap) else { return nil }
            let available = value(main, cap, supported.replacingOccurrences(of: "supported", with: "available"))?.array
            let options = (available?.isEmpty == false ? available : value(main, cap, supported)?.array)?.compactMap(\.string) ?? []
            guard !options.isEmpty else { return nil }
            return (cap, command, label, value(main, cap, attr)?.string ?? "", options, attr)
        }
        if has("thermostatCoolingSetpoint") || !choices.isEmpty {
            VStack(alignment: .leading, spacing: 20) {
                if has("thermostatCoolingSetpoint") { setpoint(main) }
                ForEach(choices, id: \.0) { cap, command, label, selected, options, _ in
                    VStack(alignment: .leading, spacing: 10) {
                        FieldLabel(text: label)
                        FlowLayout(spacing: 8) {
                            ForEach(options, id: \.self) { option in
                                Button { if option != selected { run(cap, command, [option]) } } label: {
                                    HStack(spacing: 6) {
                                        if cap == "airConditionerMode", let symbol = modeSymbol(option) { Image(systemName: symbol) }
                                        Text(Format.value(option))
                                    }
                                }
                                .buttonStyle(SegmentStyle(selected: option == selected))
                            }
                        }
                    }
                }
                displayLight(main, has)
            }
            .card(padding: 20)
        }
    }

    /// The A/C's display light. Many Samsung models report it through
    /// samsungce.airConditionerLighting but list that capability as disabled; they take it through
    /// `execute` on the OCF resource /mode/vs/0, where "Light_On" turns the light off and
    /// "Light_Off" turns it on.
    @ViewBuilder
    private func displayLight(_ main: JSONValue, _ has: (String) -> Bool) -> some View {
        if main[Self.lighting] != nil, has(Self.lighting) || has("execute") {
            let direct = has(Self.lighting)
            let supported = value(main, Self.lighting, "supportedLightingLevels")?.array?.compactMap(\.string) ?? []
            let levels = direct && !supported.isEmpty ? supported : ["on", "off"]
            let reported = value(main, Self.lighting, "lighting")?.string ?? ""
            // The A/C reports a change a moment after it happens; show the choice until then.
            let selected = lightPending.flatMap { $0.until > Date() ? $0.level : nil } ?? reported
            VStack(alignment: .leading, spacing: 10) {
                FieldLabel(text: "Display light")
                FlowLayout(spacing: 8) {
                    ForEach(levels, id: \.self) { level in
                        Button { if level != selected { setLight(level, direct: direct) } } label: {
                            Text(Format.value(level))
                        }
                        .buttonStyle(SegmentStyle(selected: level == selected))
                    }
                }
            }
        }
    }

    private func setLight(_ level: String, direct: Bool) {
        lightPending = (level, Date().addingTimeInterval(6))
        if direct {
            run(Self.lighting, "setLightingLevel", [level])
        } else {
            run("execute", "execute", ["mode/vs/0", ["x.com.samsung.da.options": [level == "off" ? "Light_On" : "Light_Off"]]])
        }
        Task {
            try? await Task.sleep(nanoseconds: 3_500_000_000)
            await load(quiet: true)
        }
    }

    private func modeSymbol(_ mode: String) -> String? {
        ["cool": "snowflake", "heat": "sun.max", "dry": "drop", "wind": "wind", "fan": "wind", "auto": "sparkles"][mode]
    }

    private func setpoint(_ main: JSONValue) -> some View {
        let unit = unit(main, "thermostatCoolingSetpoint", "coolingSetpoint")
        let range = value(main, "thermostatCoolingSetpoint", "coolingSetpointRange")
        let low = range?["minimum"]?.double ?? value(main, "custom.thermostatSetpointControl", "minimumSetpoint")?.double ?? (unit == "F" ? 60 : 16)
        let high = range?["maximum"]?.double ?? value(main, "custom.thermostatSetpointControl", "maximumSetpoint")?.double ?? (unit == "F" ? 86 : 30)
        let step = range?["step"]?.double ?? 1
        let current = target ?? value(main, "thermostatCoolingSetpoint", "coolingSetpoint")?.double ?? low
        let change = { (delta: Double) in
            touched = Date()
            Haptics.tap()
            let next = min(high, max(low, ((current + delta) / step).rounded() * step))
            target = next
            // Several taps in a row send one command.
            targetTask?.cancel()
            targetTask = Task {
                try? await Task.sleep(nanoseconds: 700_000_000)
                guard !Task.isCancelled else { return }
                run("thermostatCoolingSetpoint", "setCoolingSetpoint", [next])
                try? await Task.sleep(nanoseconds: 2_500_000_000)
                target = nil
            }
        }
        return VStack(alignment: .leading, spacing: 10) {
            FieldLabel(text: "Target temperature")
            HStack {
                stepButton("minus", "Cooler") { change(-step) }
                Spacer()
                VStack(spacing: 2) {
                    Text("\(current.formatted(.number.precision(.fractionLength(0 ... 1))))°")
                        .font(Theme.number(40, .bold))
                        .foregroundStyle(target == nil ? Theme.primary : Theme.primaryActive)
                    Text("\(unit) · \(Int(low))–\(Int(high))").font(Theme.number(12, .regular)).foregroundStyle(Theme.muted)
                }
                Spacer()
                stepButton("plus", "Warmer") { change(step) }
            }
        }
    }

    private func stepButton(_ symbol: String, _ label: String, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.system(size: 18, weight: .semibold)).foregroundStyle(Theme.strong)
                .frame(width: 48, height: 48).background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
        }
        .buttonStyle(PressableStyle())
        .accessibilityLabel(label)
    }

    @ViewBuilder
    private func laundry(_ main: JSONValue, _ has: (String) -> Bool) -> some View {
        if let cap = Self.operatingStates.first(where: has) {
            let machine = value(main, cap, "machineState")?.string ?? ""
            let supported = value(main, cap, "supportedMachineStates")?.array?.compactMap(\.string) ?? ["run", "pause", "stop"]
            let remote = value(main, "remoteControlStatus", "remoteControlEnabled")
            let locked = has("remoteControlStatus") && (remote?.string == "false" || remote == .bool(false))
            let options: [(String, String, String, String, String)] = [
                ("custom.washerSpinLevel", "washerSpinLevel", "supportedWasherSpinLevel", "setWasherSpinLevel", "Spin"),
                ("custom.washerWaterTemperature", "washerWaterTemperature", "supportedWasherWaterTemperature", "setWasherWaterTemperature", "Water temperature"),
                ("custom.washerRinseCycles", "washerRinseCycles", "supportedWasherRinseCycles", "setWasherRinseCycles", "Rinses"),
                ("custom.washerSoilLevel", "washerSoilLevel", "supportedWasherSoilLevel", "setWasherSoilLevel", "Soil level"),
            ].filter { has($0.0) }
            VStack(alignment: .leading, spacing: 16) {
                FieldLabel(text: "Cycle")
                HStack(spacing: 8) {
                    ForEach(["run", "pause", "stop"].filter(supported.contains), id: \.self) { state in
                        let symbol = ["run": "play.fill", "pause": "pause.fill", "stop": "stop.fill"][state]!
                        let button = Button { run(cap, "setMachineState", [state]) } label: { Label(Format.value(state), systemImage: symbol) }
                            .disabled(locked || state == machine || (state == "pause" && machine != "run"))
                        if state == "run" { button.buttonStyle(PrimaryButtonStyle(compact: true)) } else { button.buttonStyle(SecondaryButtonStyle(compact: true)) }
                    }
                }
                if locked {
                    Text("Remote control is off on the machine. Press its Smart Control button to start or change it from here.")
                        .font(.system(size: 13)).foregroundStyle(Theme.pending)
                }
                ForEach(options, id: \.0) { optCap, attr, supportedAttr, command, label in
                    if let values = value(main, optCap, supportedAttr)?.array?.compactMap(\.string), !values.isEmpty {
                        let selected = value(main, optCap, attr)?.string ?? ""
                        HStack {
                            Text(label).font(.system(size: 14)).foregroundStyle(Theme.text)
                            Spacer()
                            Menu {
                                ForEach(values, id: \.self) { v in
                                    Button(optCap == "custom.washerRinseCycles" ? v : Format.value(v)) { run(optCap, command, [v]) }
                                }
                            } label: {
                                HStack(spacing: 6) {
                                    Text(optCap == "custom.washerRinseCycles" ? selected : Format.value(selected))
                                    Image(systemName: "chevron.up.chevron.down").font(.system(size: 11))
                                }
                                .font(.system(size: 14, weight: .medium))
                                .foregroundStyle(Theme.strong)
                                .padding(.horizontal, 12)
                                .frame(height: 36)
                                .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
                            }
                            .disabled(locked || machine == "run")
                            .opacity(locked || machine == "run" ? 0.5 : 1)
                        }
                    }
                }
            }
            .card(padding: 20)
        }
    }

    @ViewBuilder
    private func energy(_ main: JSONValue, _ has: (String) -> Bool) -> some View {
        if has("powerConsumptionReport"), let report = value(main, "powerConsumptionReport", "powerConsumption") {
            HStack(spacing: 10) {
                Image(systemName: "bolt").foregroundStyle(Theme.muted)
                Text("\(Int(report["power"]?.double ?? 0)) W now").foregroundStyle(Theme.text)
                if let energy = report["energy"]?.double {
                    Text("\((energy / 1000).formatted(.number.precision(.fractionLength(1)))) kWh total").foregroundStyle(Theme.muted)
                }
            }
            .font(Theme.number(13, .regular))
            .padding(.horizontal, 4)
        }
    }

    @ViewBuilder
    private func everything(_ main: JSONValue, _ disabled: Set<String>) -> some View {
        let rows: [(String, String)] = {
            guard case let .object(caps) = main else { return [] }
            var out: [(String, String)] = []
            for (cap, attrs) in caps.sorted(by: { $0.key < $1.key }) where !Self.handled.contains(cap) && !disabled.contains(cap) {
                guard case let .object(attributes) = attrs else { continue }
                for (attr, entry) in attributes.sorted(by: { $0.key < $1.key }) {
                    let text = entry["value"]?.display ?? ""
                    if !text.isEmpty { out.append(("\(cap) · \(attr)", text + (entry["unit"]?.string.map { " \($0)" } ?? ""))) }
                }
            }
            return out
        }()
        if !rows.isEmpty {
            DisclosureGroup("Everything SmartThings reports") {
                VStack(alignment: .leading, spacing: 8) {
                    ForEach(rows, id: \.0) { key, value in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(key).foregroundStyle(Theme.muted)
                            Text(value).foregroundStyle(Theme.text).textSelection(.enabled)
                        }
                        .font(.system(size: 11, design: .monospaced))
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.top, 10)
            }
            .font(.system(size: 14, weight: .semibold))
            .tint(Theme.mutedStrong)
        }
    }
}

struct SegmentStyle: ButtonStyle {
    var selected: Bool
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 14, weight: .semibold))
            .padding(.horizontal, 14)
            .frame(height: 36)
            .foregroundStyle(selected ? Theme.strong : Theme.mutedStrong)
            .background(RoundedRectangle(cornerRadius: 6).fill(selected ? Theme.raised : .clear))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(selected ? Theme.primary : Theme.hairline))
            .opacity(configuration.isPressed ? 0.7 : 1)
    }
}

struct PowerToggleStyle: ButtonStyle {
    var on: Bool
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 15, weight: .semibold))
            .padding(.horizontal, 16)
            .frame(height: 40)
            .foregroundStyle(on ? Theme.onPrimary : Theme.mutedStrong)
            .background(RoundedRectangle(cornerRadius: 6).fill(on ? Theme.primary : .clear))
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(on ? Theme.primary : Theme.hairline))
            .opacity(configuration.isPressed ? 0.8 : 1)
    }
}
