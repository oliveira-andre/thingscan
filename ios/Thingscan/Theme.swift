import SwiftUI

/// DESIGN.md tokens: a near-black canvas, flat card surfaces, one yellow accent for primary
/// actions and key numbers, green/red for up/down as text only.
enum Theme {
    static let canvas = Color(hex: 0x0B0E11)
    static let card = Color(hex: 0x1E2329)
    static let raised = Color(hex: 0x2B3139)
    static let hairline = Color(hex: 0x2B3139)
    static let text = Color(hex: 0xEAECEF)
    static let strong = Color.white
    static let muted = Color(hex: 0x707A8A)
    static let mutedStrong = Color(hex: 0x929AA5)
    static let primary = Color(hex: 0xFCD535)
    static let primaryActive = Color(hex: 0xF0B90B)
    static let onPrimary = Color(hex: 0x181A20)
    static let up = Color(hex: 0x0ECB81)
    static let down = Color(hex: 0xF6465D)
    static let pending = Color(hex: 0xF0B90B)

    static func number(_ size: CGFloat, _ weight: Font.Weight = .semibold) -> Font {
        .system(size: size, weight: weight).monospacedDigit()
    }
}

extension Color {
    init(hex: UInt32) {
        self.init(
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255
        )
    }
}

/// Black on yellow, the system's main action.
struct PrimaryButtonStyle: ButtonStyle {
    var pill = false
    var compact = false
    @Environment(\.isEnabled) private var enabled

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: compact ? 13 : 15, weight: .semibold))
            .padding(.horizontal, compact ? 12 : 20)
            .frame(height: compact ? 32 : 44)
            .foregroundStyle(enabled ? Theme.onPrimary : Theme.muted)
            .background(
                RoundedRectangle(cornerRadius: pill ? 22 : 6)
                    .fill(enabled ? (configuration.isPressed ? Theme.primaryActive : Theme.primary) : Theme.primary.opacity(0.3))
            )
    }
}

struct SecondaryButtonStyle: ButtonStyle {
    var compact = false
    var destructive = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: compact ? 13 : 15, weight: .semibold))
            .padding(.horizontal, compact ? 12 : 18)
            .frame(height: compact ? 32 : 44)
            .foregroundStyle(destructive ? Theme.down : Theme.strong)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(configuration.isPressed ? Theme.raised : Theme.card)
            )
            .overlay(RoundedRectangle(cornerRadius: 6).stroke(Theme.hairline))
    }
}

struct CardModifier: ViewModifier {
    var padding: CGFloat = 20
    func body(content: Content) -> some View {
        content
            .padding(padding)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(RoundedRectangle(cornerRadius: 12).fill(Theme.card))
    }
}

extension View {
    func card(padding: CGFloat = 20) -> some View { modifier(CardModifier(padding: padding)) }
}
