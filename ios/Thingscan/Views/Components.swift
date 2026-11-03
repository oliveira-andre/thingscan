import SwiftUI

// MARK: Formatting

enum Format {
    static func ago(_ seconds: UInt64) -> String {
        let delta = max(0, Date().timeIntervalSince1970 - TimeInterval(seconds))
        if delta < 45 { return "just now" }
        if delta < 3600 { return "\(Int((delta / 60).rounded())) min ago" }
        if delta < 86400 { return "\(Int((delta / 3600).rounded())) h ago" }
        let days = Int((delta / 86400).rounded())
        return "\(days) day\(days == 1 ? "" : "s") ago"
    }

    static func ago(_ date: Date) -> String { ago(UInt64(date.timeIntervalSince1970)) }

    static func clock(_ date: Date) -> String { date.formatted(date: .omitted, time: .shortened) }

    static func subtitle(manufacturer: String?, model: String?, kind: Kind, details: [String: String]) -> String {
        let made = [manufacturer, model].compactMap { $0 }.joined(separator: " · ")
        if !made.isEmpty { return made }
        if let mnid = detail(details, "st.mnid") { return "SmartThings device · model \(mnid)" }
        if let vendor = detail(details, "matter.vendor_id") { return "Matter · vendor \(vendor)" }
        return kind.label
    }

    static func detail(_ details: [String: String], _ key: String) -> String? {
        details[key] ?? details.first { $0.key.hasSuffix(".\(key)") }?.value
    }

    /// "AirConditioner" → "Air conditioner".
    static func category(_ category: String?) -> String {
        guard let category else { return "Device" }
        let spaced = category.replacingOccurrences(of: "([a-z])([A-Z])", with: "$1 $2", options: .regularExpression).lowercased()
        return spaced.prefix(1).uppercased() + spaced.dropFirst()
    }

    private static let valueLabels: [String: String] = [
        "wind": "Fan", "windFree": "WindFree", "auto": "Auto", "fixed": "Fixed", "all": "All", "vertical": "Up & down",
        "horizontal": "Side to side", "run": "Start", "pause": "Pause", "stop": "Stop", "none": "None", "noSpin": "No spin",
        "tapCold": "Tap cold",
    ]

    /// A SmartThings enum value for people: "extraHigh" → "Extra high".
    static func value(_ raw: String) -> String {
        if let label = valueLabels[raw] { return label }
        let spaced = raw.replacingOccurrences(of: "([a-z0-9])([A-Z])", with: "$1 $2", options: .regularExpression)
            .replacingOccurrences(of: "_", with: " ").lowercased()
        return spaced.prefix(1).uppercased() + spaced.dropFirst()
    }
}

extension SavedDevice {
    var subtitle: String { Format.subtitle(manufacturer: manufacturer, model: model, kind: kind, details: details) }
}

// MARK: Status

enum Tone {
    case checking, reachable, unreachable, unknown, idle

    var color: Color {
        switch self {
        case .reachable: Theme.up
        case .unreachable: Theme.down
        case .unknown, .checking: Theme.pending
        case .idle: Theme.mutedStrong
        }
    }
}

func status(of check: CheckState?, device: SavedDevice) -> (tone: Tone, text: String) {
    let seen = device.lastSeen.map { "seen \(Format.ago($0))" } ?? "never seen since added"
    guard let check else { return (.idle, device.lastSeen == nil ? "Not checked yet" : "Not checked yet · \(seen)") }
    guard case let .done(report) = check else { return (.checking, "Checking…") }
    switch report.status {
    case .reachable:
        let ok = report.probes.filter { $0.status == .reachable }
        // A TV in standby still shows as online in SmartThings, so don't call it reachable here.
        if !ok.isEmpty, ok.allSatisfy({ $0.via == "SmartThings" }) { return (.reachable, "Online on API") }
        let fastest = ok.compactMap(\.latencyMs).min()
        let extra = fastest.map { "\($0) ms" } ?? ok.first?.detail
        return (.reachable, extra.map { "Reachable · \($0)" } ?? "Reachable")
    case .unreachable:
        return (.unreachable, "Not responding · \(seen)")
    case .unknown:
        return (.unknown, "Can't check · \(report.probes.first?.detail ?? "nothing to check")")
    }
}

struct StatusDot: View {
    var tone: Tone
    var body: some View {
        if tone == .checking {
            ProgressView().controlSize(.mini).tint(Theme.mutedStrong)
        } else {
            Circle().fill(tone == .idle ? Theme.muted : tone.color).frame(width: 8, height: 8)
        }
    }
}

struct Chip: View {
    var label: String
    var status: ReachStatus?
    var body: some View {
        Text(label)
            .font(.system(size: 12, weight: .medium))
            .padding(.horizontal, 8)
            .frame(height: 22)
            .foregroundStyle(status.map { s -> Color in
                switch s {
                case .reachable: Theme.up
                case .unreachable: Theme.down
                case .unknown: Theme.pending
                }
            } ?? Theme.mutedStrong)
            .background(RoundedRectangle(cornerRadius: 4).fill(Theme.raised))
    }
}

// MARK: Icons

enum DeviceIcon {
    static func symbol(kind: Kind, name: String, details: [String: String]) -> String {
        let hints = [name, Format.detail(details, "ocf.device_types"), Format.detail(details, "matter.device_type"), details["cloud.category"]]
            .compactMap { $0 }.joined(separator: " ").lowercased()
        let rules: [(String, String)] = [
            ("airconditioner|air.?condition|a/c\\b|\\bac\\b", "air.conditioner.horizontal"),
            ("washer|laundry|clothingcare", "washer"),
            ("dryer", "dryer"),
            ("refrigerator|fridge", "refrigerator"),
            ("dishwasher", "dishwasher"),
            ("oven|cooktop|range|microwave", "oven"),
            ("robot|vacuum|cleaner", "robotic.vacuum"),
            ("thermostat|temperature", "thermometer.medium"),
            ("purifier|\\bfan\\b", "fan"),
            ("light|lamp|bulb", "lightbulb"),
            ("plug|outlet|switch", "powerplug"),
            ("lock", "lock"),
            ("speaker", "hifispeaker"),
            ("\\btv\\b|television|oic\\.d\\.tv|video player", "tv"),
            ("\\bhub\\b|bridge", "wifi.router"),
        ]
        for (pattern, symbol) in rules where hints.range(of: pattern, options: .regularExpression) != nil {
            return symbol
        }
        switch kind {
        case .readyToAdd: return "sparkles"
        case .hub: return "wifi.router"
        case .tv: return "tv"
        case .threadBorderRouter: return "antenna.radiowaves.left.and.right"
        case .tracker: return "tag"
        case .samsung: return "dot.radiowaves.left.and.right"
        default: return "cpu"
        }
    }
}

struct KindTile: View {
    var kind: Kind
    var name: String
    var details: [String: String]
    var size: CGFloat = 40

    var body: some View {
        Image(systemName: DeviceIcon.symbol(kind: kind, name: name, details: details))
            .font(.system(size: size * 0.45, weight: .medium))
            .foregroundStyle(kind == .readyToAdd ? Theme.primary : Theme.strong)
            .frame(width: size, height: size)
            .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
    }
}

// MARK: Layout

/// Lays children out in rows, wrapping when the width runs out (mode and option buttons).
struct FlowLayout: Layout {
    var spacing: CGFloat = 8

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > width {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: proposal.width ?? x, height: y + rowHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

/// Text tabs with a yellow bar under the selected one.
struct TabBar<T: Hashable>: View {
    var tabs: [(T, String)]
    @Binding var selection: T

    var body: some View {
        HStack(spacing: 24) {
            ForEach(tabs, id: \.0) { tab, label in
                Button {
                    selection = tab
                } label: {
                    VStack(spacing: 8) {
                        Text(label)
                            .font(.system(size: 15, weight: .semibold))
                            .foregroundStyle(selection == tab ? Theme.strong : Theme.muted)
                        Rectangle().fill(selection == tab ? Theme.primary : .clear).frame(height: 2)
                    }
                    .fixedSize(horizontal: true, vertical: false)
                }
                .buttonStyle(.plain)
            }
            Spacer()
        }
        .overlay(alignment: .bottom) { Rectangle().fill(Theme.hairline).frame(height: 1) }
    }
}

struct Callout<Action: View>: View {
    var symbol: String
    var title: String
    var message: String
    @ViewBuilder var action: Action

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Image(systemName: symbol)
                .font(.system(size: 18, weight: .medium))
                .foregroundStyle(Theme.primary)
                .frame(width: 40, height: 40)
                .background(RoundedRectangle(cornerRadius: 8).fill(Theme.raised))
            Text(title).font(.system(size: 16, weight: .semibold)).foregroundStyle(Theme.strong)
            Text(message).font(.system(size: 14)).foregroundStyle(Theme.muted)
            action.padding(.top, 4)
        }
        .card(padding: 20)
    }
}

/// Light form sheets (DESIGN.md's transactional surfaces).
/// Sheets (add, edit, sign in…) sit on a card surface, dark like the rest of the app.
struct FormSheet: ViewModifier {
    func body(content: Content) -> some View {
        content
            .presentationBackground(Theme.card)
            .presentationDragIndicator(.visible)
    }
}

extension View {
    func formSheet() -> some View { modifier(FormSheet()) }
}

struct FieldLabel: View {
    var text: String
    var body: some View {
        Text(text).font(.system(size: 12, weight: .medium)).foregroundStyle(Theme.muted)
    }
}

struct FormField: View {
    var placeholder: String
    @Binding var text: String
    var monospaced = false
    var secure = false

    var body: some View {
        Group {
            if secure {
                SecureField(placeholder, text: $text)
            } else {
                TextField(placeholder, text: $text)
            }
        }
        .font(monospaced ? .system(size: 14, design: .monospaced) : .system(size: 16))
        .foregroundStyle(Theme.strong)
        .padding(.horizontal, 14)
        .frame(height: 44)
        .background(RoundedRectangle(cornerRadius: 6).fill(Theme.raised))
    }
}

enum Haptics {
    static func tap() { UIImpactFeedbackGenerator(style: .light).impactOccurred() }
    static func press() { UIImpactFeedbackGenerator(style: .medium).impactOccurred() }
}
