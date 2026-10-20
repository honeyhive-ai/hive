import SwiftUI

struct ChatView: View {
    @Environment(AppModel.self) private var model
    let chatId: String
    @State private var draft = ""
    @State private var sending = false
    @FocusState private var composerFocused: Bool

    private var messages: [Message] { model.messages[chatId] ?? [] }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if messages.isEmpty {
                        ContentUnavailableView("Say hello", systemImage: "text.bubble",
                                               description: Text("Mention an agent with @ to get it working."))
                            .padding(.top, 80)
                    }
                    ForEach(messages) { m in
                        MessageBubble(message: m, agent: model.agent(id: m.agentId))
                            .id(m.id)
                    }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, 14)
                .padding(.top, 8)
            }
            .defaultScrollAnchor(.bottom)
            .scrollDismissesKeyboard(.interactively)
            .onChange(of: messages.last?.body) { _, _ in
                withAnimation(.easeOut(duration: 0.15)) { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .onChange(of: messages.count) { _, _ in
                withAnimation { proxy.scrollTo("bottom", anchor: .bottom) }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                VStack(spacing: 8) {
                    TurnStatusStrip(turns: model.activeTurns(chatId: chatId))
                    ComposerView(draft: $draft, sending: sending, focused: $composerFocused, agents: model.agents) {
                        Task { await send() }
                    }
                }
                .padding(.horizontal, 12)
                .padding(.bottom, 8)
            }
        }
        .navigationTitle(model.chat(id: chatId)?.title ?? "Chat")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { ToolbarItem(placement: .topBarTrailing) { StreamBadge() } }
        .toolbar(.hidden, for: .tabBar)
        .task(id: chatId) {
            await model.loadMessages(chatId: chatId)
            if model.agents.isEmpty { await model.refreshAgents() }
        }
        .refreshable { await model.loadMessages(chatId: chatId) }
    }

    private func send() async {
        let text = draft
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        sending = true
        defer { sending = false }
        if await model.send(chatId: chatId, body: text) {
            draft = ""
        }
    }
}

struct MessageBubble: View {
    let message: Message
    let agent: Agent?

    var body: some View {
        let isUser = message.role == .user
        if message.role == .system {
            Text(message.body)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity)
                .multilineTextAlignment(.center)
                .padding(.vertical, 4)
        } else {
            HStack(alignment: .bottom) {
                if isUser { Spacer(minLength: 40) }
                VStack(alignment: isUser ? .trailing : .leading, spacing: 4) {
                    header(isUser: isUser)
                    VStack(alignment: .leading, spacing: 6) {
                        if message.body.isEmpty && message.streaming {
                            TypingDots()
                        } else {
                            MarkdownView(text: message.body, streaming: message.streaming)
                        }
                    }
                    .padding(.horizontal, 13)
                    .padding(.vertical, 10)
                    .background(isUser ? Theme.userBubble : Theme.agentBubble,
                                in: .rect(cornerRadius: 18, style: .continuous))
                    .textSelection(.enabled)
                }
                if !isUser { Spacer(minLength: 24) }
            }
        }
    }

    @ViewBuilder
    private func header(isUser: Bool) -> some View {
        HStack(spacing: 6) {
            if !isUser {
                Image(systemName: "sparkle").font(.caption2).foregroundStyle(Theme.accent)
                Text(agent.map { "@\($0.name)" } ?? message.author).font(.caption.weight(.semibold))
                if let model = agent?.model {
                    Text(model).font(.caption2.monospaced()).foregroundStyle(.secondary)
                }
            } else {
                Text(message.author.isEmpty ? "You" : message.author).font(.caption.weight(.semibold))
            }
            Text(message.createdAt, style: .time).font(.caption2).foregroundStyle(.tertiary)
            if message.streaming {
                ProgressView().controlSize(.mini)
            }
        }
        .foregroundStyle(.secondary)
    }
}

struct TypingDots: View {
    var body: some View {
        TimelineView(.animation(minimumInterval: 0.3)) { ctx in
            let phase = Int(ctx.date.timeIntervalSinceReferenceDate / 0.3) % 3
            HStack(spacing: 5) {
                ForEach(0..<3) { i in
                    Circle().frame(width: 7, height: 7).opacity(i == phase ? 1 : 0.35)
                }
            }
            .foregroundStyle(.secondary)
            .padding(.vertical, 4)
        }
        .accessibilityLabel("Typing")
    }
}

struct TurnStatusStrip: View {
    @Environment(AppModel.self) private var model
    let turns: [TurnStatus]

    var body: some View {
        if !turns.isEmpty {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(turns, id: \.agentId) { t in
                    let name = model.agent(id: t.agentId)?.name ?? t.agentId
                    HStack(spacing: 8) {
                        switch t.state {
                        case .error:
                            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                            Text("\(name) failed\(t.detail.map { ": \($0)" } ?? "")")
                        case .queued:
                            Image(systemName: "hourglass").foregroundStyle(.secondary)
                            Text("\(name) is queued…")
                        default:
                            ProgressView().controlSize(.small)
                            Text("\(name) is working…")
                        }
                    }
                    .font(.footnote)
                    .lineLimit(2)
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .glassEffect(.regular, in: .rect(cornerRadius: 16))
            .transition(.opacity.combined(with: .move(edge: .bottom)))
        }
    }
}

struct ComposerView: View {
    @Binding var draft: String
    let sending: Bool
    var focused: FocusState<Bool>.Binding
    let agents: [Agent]
    let onSend: () -> Void

    /// The `@partial` being typed at the end of the draft, if any.
    private var mentionQuery: String? { MentionHelper.activeQuery(in: draft) }

    private var suggestions: [Agent] {
        guard let q = mentionQuery else { return [] }
        return agents.filter { q.isEmpty || $0.name.localizedCaseInsensitiveContains(q) }
    }

    var body: some View {
        VStack(spacing: 8) {
            if !suggestions.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(suggestions) { a in
                            Button("@\(a.name)") { draft = MentionHelper.insert(a.name, into: draft) }
                                .buttonStyle(.glass)
                                .controlSize(.small)
                        }
                    }
                    .padding(.horizontal, 4)
                }
            }
            GlassEffectContainer {
                HStack(alignment: .bottom, spacing: 8) {
                    Menu {
                        ForEach(agents) { a in
                            Button {
                                draft = MentionHelper.insert(a.name, into: draft)
                            } label: {
                                Label("@\(a.name)", systemImage: "sparkle")
                                Text([a.model, a.hostLabel].compactMap { $0 }.joined(separator: " · "))
                            }
                        }
                    } label: {
                        Image(systemName: "at").font(.title3).frame(width: 40, height: 40)
                    }
                    .glassEffect(.regular.interactive(), in: .circle)
                    .accessibilityLabel("Mention an agent")

                    TextField("Message", text: $draft, axis: .vertical)
                        .lineLimit(1...6)
                        .focused(focused)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .glassEffect(.regular, in: .rect(cornerRadius: 20))

                    Button(action: onSend) {
                        Group {
                            if sending { ProgressView() } else { Image(systemName: "arrow.up").font(.headline) }
                        }
                        .frame(width: 40, height: 40)
                    }
                    .glassEffect(.regular.tint(Theme.accent).interactive(), in: .circle)
                    .foregroundStyle(.black)
                    .opacity(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && !sending ? 0.45 : 1)
                    .disabled(sending || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityLabel("Send")
                }
            }
        }
    }
}

/// Pure helpers for `@Name` mentions (unit-tested).
enum MentionHelper {
    /// If the draft ends with `@partial` (no whitespace after `@`), returns `partial`.
    static func activeQuery(in draft: String) -> String? {
        guard let at = draft.lastIndex(of: "@") else { return nil }
        if at > draft.startIndex {
            let before = draft[draft.index(before: at)]
            guard before.isWhitespace else { return nil } // e.g. an email address
        }
        let partial = draft[draft.index(after: at)...]
        guard !partial.contains(where: \.isWhitespace) else { return nil }
        return String(partial)
    }

    /// Completes a trailing `@partial`, or appends `@Name ` with sensible spacing.
    static func insert(_ name: String, into draft: String) -> String {
        if let q = activeQuery(in: draft) {
            return String(draft.dropLast(q.count + 1)) + "@\(name) "
        }
        if draft.isEmpty || draft.last?.isWhitespace == true { return draft + "@\(name) " }
        return draft + " @\(name) "
    }
}
