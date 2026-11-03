import SwiftUI

@main
struct ThingscanApp: App {
    @State private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            HomeView()
                .environment(model)
                .preferredColorScheme(.dark)
                .tint(Theme.primary)
        }
        .onChange(of: scenePhase) { _, phase in
            // Opening the app (or coming back to it) syncs and checks every device.
            if phase == .active { Task { await model.refreshAll() } }
            model.setActive(phase == .active)
        }
    }
}
