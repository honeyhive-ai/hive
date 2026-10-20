import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        Group {
            if model.isPaired {
                MainTabs()
            } else {
                NavigationStack { PairingView() }
            }
        }
        .sheet(item: $model.pendingPair) { cfg in
            NavigationStack { PairConfirmView(candidate: cfg) }
                .presentationDetents([.medium])
        }
        .overlay(alignment: .top) {
            if let banner = model.banner {
                BannerView(text: banner) { model.banner = nil }
                    .padding(.horizontal, 16)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    .task(id: banner) {
                        try? await Task.sleep(for: .seconds(5))
                        if model.banner == banner { model.banner = nil }
                    }
            }
        }
        .animation(.snappy, value: model.banner)
    }
}

extension ServerConfig: Identifiable {
    var id: String { baseURL.absoluteString + "#" + token.prefix(6) }
}

struct MainTabs: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        TabView(selection: $model.tab) {
            Tab("Chats", systemImage: "bubble.left.and.text.bubble.right", value: AppTab.chats) {
                ChatsView()
            }
            .badge(model.chats.reduce(0) { $0 + $1.unread })
            Tab("Agents", systemImage: "cpu", value: AppTab.agents) {
                AgentsView()
            }
            Tab("Review", systemImage: "checklist", value: AppTab.review) {
                ReviewView()
            }
            .badge(model.proposals.count)
            Tab("Lab", systemImage: "gauge.with.dots.needle.67percent", value: AppTab.lab) {
                LabView()
            }
            Tab("Settings", systemImage: "gearshape", value: AppTab.settings) {
                SettingsView()
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
        .task { await model.refreshAll() }
    }
}

struct BannerView: View {
    let text: String
    let dismiss: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.yellow)
            Text(text).font(.callout).frame(maxWidth: .infinity, alignment: .leading)
            Button(action: dismiss) { Image(systemName: "xmark").font(.caption.bold()) }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss")
        }
        .padding(14)
        .glassEffect(.regular, in: .rect(cornerRadius: 18))
    }
}

/// Small pill showing the live-stream state.
struct StreamBadge: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let (color, label): (Color, String) = switch model.streamState {
        case .live: (.green, "Live")
        case .connecting: (.orange, "Connecting")
        case .idle: (.gray, "Paused")
        case .offline: (.red, "Offline")
        }
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 7, height: 7)
            Text(label).font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Server stream \(label)")
    }
}
