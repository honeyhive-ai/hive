import SwiftUI

@main
struct HiveApp: App {
    @State private var model: AppModel
    @Environment(\.scenePhase) private var scenePhase

    init() {
        // Unit tests host the app; keep it inert so tests own the network.
        let testing = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
        _model = State(initialValue: testing
            ? AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
            : AppModel())
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .preferredColorScheme(.dark)
                .tint(Theme.accent)
                .onOpenURL { model.handle(url: $0) }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .active: model.resume()
            case .background: model.suspend()
            default: break
            }
        }
    }
}

enum Theme {
    static let accent = Color("AccentColor")
    static let userBubble = Color.accentColor.opacity(0.22)
    static let agentBubble = Color(white: 0.14)
    static let codeBackground = Color(white: 0.08)
    static let added = Color(red: 0.25, green: 0.78, blue: 0.42)
    static let removed = Color(red: 0.95, green: 0.36, blue: 0.36)
    static let hunk = Color(red: 0.55, green: 0.55, blue: 1.0)
}
