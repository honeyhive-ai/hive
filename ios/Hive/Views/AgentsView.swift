import SwiftUI

struct AgentsView: View {
    @Environment(AppModel.self) private var model
    @State private var moving: String?

    var body: some View {
        NavigationStack {
            List {
                Section("Agents") {
                    ForEach(model.agents) { agent in
                        AgentRow(agent: agent, hosts: model.hosts, busy: moving == agent.id) { host in
                            Task {
                                moving = agent.id
                                await model.sendAgent(agent, to: host)
                                moving = nil
                            }
                        }
                    }
                }
                Section("Hosts") {
                    ForEach(model.hosts) { host in
                        HStack {
                            Image(systemName: host.isThis ? "server.rack" : "laptopcomputer")
                                .foregroundStyle(.secondary)
                                .frame(width: 28)
                            VStack(alignment: .leading) {
                                Text(host.label)
                                if host.isThis {
                                    Text("This home host").font(.caption).foregroundStyle(.secondary)
                                }
                            }
                            Spacer()
                            Label(host.online ? "Online" : "Offline", systemImage: "circle.fill")
                                .labelStyle(StatusDotLabelStyle(color: host.online ? .green : .gray))
                        }
                    }
                }
            }
            .overlay {
                if model.agents.isEmpty && model.hosts.isEmpty {
                    ContentUnavailableView("No agents", systemImage: "cpu",
                                           description: Text("Add agents from the desktop app or the hive CLI."))
                }
            }
            .refreshable { await model.refreshAgents() }
            .navigationTitle("Agents")
            .task { await model.refreshAgents() }
        }
    }
}

struct AgentRow: View {
    let agent: Agent
    let hosts: [Host]
    let busy: Bool
    let onMove: (Host) -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 10).fill(Theme.accent.opacity(0.16))
                Image(systemName: "sparkle").foregroundStyle(Theme.accent)
            }
            .frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 3) {
                Text("@\(agent.name)").font(.body.weight(.semibold))
                Text([agent.runtimeLabel, agent.model].compactMap { $0 }.joined(separator: " · "))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer()
            Menu {
                Section("Send to host") {
                    ForEach(hosts) { host in
                        Button {
                            onMove(host)
                        } label: {
                            if host.id == agent.hostId {
                                Label(host.label, systemImage: "checkmark")
                            } else {
                                Text(host.label + (host.online ? "" : " (offline)"))
                            }
                        }
                        .disabled(host.id == agent.hostId)
                    }
                }
            } label: {
                HStack(spacing: 4) {
                    if busy { ProgressView().controlSize(.small) }
                    Text(agent.hostLabel ?? "Unassigned").font(.caption.weight(.medium))
                    Image(systemName: "chevron.up.chevron.down").font(.caption2)
                }
                .padding(.horizontal, 10).padding(.vertical, 6)
            }
            .glassEffect(.regular.interactive(), in: .capsule)
            .accessibilityLabel("Host for \(agent.name): \(agent.hostLabel ?? "unassigned"). Send to host.")
        }
        .padding(.vertical, 2)
    }
}

struct StatusDotLabelStyle: LabelStyle {
    let color: Color
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 5) {
            Circle().fill(color).frame(width: 8, height: 8)
            configuration.title.font(.caption).foregroundStyle(.secondary)
        }
    }
}
