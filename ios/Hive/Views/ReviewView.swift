import SwiftUI

struct ReviewView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.reviewPath) {
            List(model.proposals) { p in
                NavigationLink(value: p.id) { ProposalRow(proposal: p) }
            }
            .listStyle(.plain)
            .overlay {
                if model.proposals.isEmpty {
                    ContentUnavailableView("Nothing to review", systemImage: "checkmark.seal",
                                           description: Text("Agent proposals waiting for approval show up here."))
                }
            }
            .refreshable { await model.refreshProposals() }
            .navigationTitle("Review")
            .navigationDestination(for: String.self) { id in ProposalDetailView(proposalId: id) }
            .task { await model.refreshProposals() }
        }
    }
}

struct ProposalRow: View {
    let proposal: Proposal

    var body: some View {
        let stats = DiffParser.stats(proposal.diff ?? "")
        VStack(alignment: .leading, spacing: 4) {
            Text(proposal.title).font(.body.weight(.semibold)).lineLimit(2)
            Text(proposal.body).font(.subheadline).foregroundStyle(.secondary).lineLimit(2)
            HStack(spacing: 10) {
                Label("@\(proposal.author)", systemImage: "sparkle").font(.caption)
                Text(proposal.kind).font(.caption.monospaced())
                    .padding(.horizontal, 6).padding(.vertical, 1)
                    .background(.fill.tertiary, in: .capsule)
                if stats.added + stats.removed > 0 {
                    Text("+\(stats.added)").foregroundStyle(Theme.added)
                    Text("−\(stats.removed)").foregroundStyle(Theme.removed)
                }
                Spacer()
                Text(proposal.createdAt, format: .relative(presentation: .named, unitsStyle: .abbreviated))
            }
            .font(.caption.monospacedDigit())
            .foregroundStyle(.secondary)
        }
        .padding(.vertical, 4)
    }
}

struct ProposalDetailView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let proposalId: String
    @State private var showReject = false
    @State private var reason = ""
    @State private var busy = false

    private var proposal: Proposal? { model.proposals.first { $0.id == proposalId } }

    var body: some View {
        Group {
            if let p = proposal {
                ScrollView {
                    VStack(alignment: .leading, spacing: 16) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(p.title).font(.title3.bold())
                            HStack(spacing: 8) {
                                Text("@\(p.author)")
                                Text("·")
                                Text(p.createdAt, format: .relative(presentation: .named))
                                if let chat = model.chat(id: p.chatId) {
                                    Text("·")
                                    Button(chat.title) {
                                        model.tab = .chats
                                        model.chatPath = [chat.id]
                                    }
                                    .font(.subheadline)
                                }
                            }
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                        }
                        if !p.body.isEmpty { MarkdownView(text: p.body) }
                        if let diff = p.diff, !diff.isEmpty {
                            DiffView(diff: diff)
                        }
                    }
                    .padding(16)
                    .padding(.bottom, 80)
                }
                .safeAreaInset(edge: .bottom) {
                    GlassEffectContainer(spacing: 12) {
                        HStack(spacing: 12) {
                            Button(role: .destructive) { showReject = true } label: {
                                Label("Reject", systemImage: "xmark").frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.glass)
                            Button { Task { await approve(p) } } label: {
                                Label("Approve", systemImage: "checkmark").frame(maxWidth: .infinity)
                            }
                            .buttonStyle(.glassProminent)
                        }
                        .controlSize(.large)
                        .disabled(busy)
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
                }
                .alert("Reject proposal", isPresented: $showReject) {
                    TextField("Reason (optional)", text: $reason)
                    Button("Cancel", role: .cancel) {}
                    Button("Reject", role: .destructive) { Task { await reject(p) } }
                } message: {
                    Text("The agent sees your reason in the chat.")
                }
            } else {
                ContentUnavailableView("Proposal resolved", systemImage: "checkmark.seal")
            }
        }
        .navigationTitle("Proposal")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
    }

    private func approve(_ p: Proposal) async {
        busy = true
        defer { busy = false }
        if await model.approve(p) { dismiss() }
    }

    private func reject(_ p: Proposal) async {
        busy = true
        defer { busy = false }
        if await model.reject(p, reason: reason) {
            reason = ""
            dismiss()
        }
    }
}
