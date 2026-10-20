import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var model
    @State private var confirmForget = false
    @State private var showRepair = false

    var body: some View {
        NavigationStack {
            Form {
                Section("Server") {
                    LabeledContent("URL") {
                        Text(model.config?.baseURL.absoluteString ?? "—")
                            .font(.callout.monospaced())
                            .textSelection(.enabled)
                    }
                    LabeledContent("Token", value: model.config.map { Self.mask($0.token) } ?? "—")
                    if let h = model.health {
                        LabeledContent("Workspace", value: h.workspace ?? "—")
                        LabeledContent("Device", value: h.deviceLabel ?? "—")
                        LabeledContent("Server version", value: h.version)
                    }
                    LabeledContent("Live updates") {
                        VStack(alignment: .trailing, spacing: 2) {
                            StreamBadge()
                            if case let .offline(reason) = model.streamState {
                                Text(reason).font(.caption2).foregroundStyle(.secondary)
                                    .multilineTextAlignment(.trailing)
                            }
                        }
                    }
                    if model.isMock {
                        Label("Mock server (fixture data)", systemImage: "theatermasks")
                            .foregroundStyle(.orange)
                    }
                }

                Section {
                    Button("Reconnect", systemImage: "arrow.clockwise") {
                        model.startEvents()
                        Task { await model.refreshAll() }
                    }
                    Button("Re-pair…", systemImage: "qrcode") { showRepair = true }
                    Button("Forget this server", systemImage: "trash", role: .destructive) { confirmForget = true }
                } footer: {
                    Text("Notifications arrive through the ntfy app; tapping one opens the chat here.")
                }

                Section("About") {
                    LabeledContent("App version",
                                   value: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "—")
                }
            }
            .navigationTitle("Settings")
            .task { await model.refreshHealth() }
            .confirmationDialog("Forget this server?", isPresented: $confirmForget, titleVisibility: .visible) {
                Button("Forget", role: .destructive) { model.forget() }
            } message: {
                Text("The URL and token are removed from the Keychain. You'll need to pair again.")
            }
            .sheet(isPresented: $showRepair) {
                NavigationStack {
                    PairingView()
                        .toolbar {
                            ToolbarItem(placement: .cancellationAction) {
                                Button("Cancel", role: .cancel) { showRepair = false }
                            }
                        }
                }
                .onChange(of: model.config) { _, _ in showRepair = false }
            }
        }
    }

    static func mask(_ token: String) -> String {
        guard token.count > 8 else { return String(repeating: "•", count: token.count) }
        return String(token.prefix(4)) + "••••" + String(token.suffix(2))
    }
}
