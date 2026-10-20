import SwiftUI

struct ChatsView: View {
    @Environment(AppModel.self) private var model
    @State private var showNew = false
    @State private var newTitle = ""

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.chatPath) {
            List(model.chats) { chat in
                NavigationLink(value: chat.id) { ChatRow(chat: chat) }
            }
            .listStyle(.plain)
            .overlay {
                if model.chats.isEmpty {
                    ContentUnavailableView("No chats yet", systemImage: "bubble.left.and.bubble.right",
                                           description: Text("Start one with the compose button."))
                }
            }
            .refreshable { await model.refreshChats() }
            .navigationTitle("Chats")
            .navigationDestination(for: String.self) { id in ChatView(chatId: id) }
            .toolbar {
                ToolbarItem(placement: .topBarLeading) { StreamBadge() }
                ToolbarItem(placement: .topBarTrailing) {
                    Button { newTitle = ""; showNew = true } label: { Image(systemName: "square.and.pencil") }
                        .accessibilityLabel("New chat")
                }
            }
            .alert("New chat", isPresented: $showNew) {
                TextField("Title (optional)", text: $newTitle)
                Button("Cancel", role: .cancel) {}
                Button("Create") {
                    Task {
                        let title = newTitle.trimmingCharacters(in: .whitespacesAndNewlines)
                        if let chat = await model.createChat(title: title.isEmpty ? nil : title) {
                            model.chatPath = [chat.id]
                        }
                    }
                }
            }
        }
    }
}

struct ChatRow: View {
    let chat: Chat

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            ZStack {
                Circle().fill(Theme.accent.opacity(0.18))
                Text(String(chat.title.first ?? "#").uppercased())
                    .font(.headline)
                    .foregroundStyle(Theme.accent)
            }
            .frame(width: 42, height: 42)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .firstTextBaseline) {
                    Text(chat.title.isEmpty ? "Untitled" : chat.title)
                        .font(.body.weight(chat.unread > 0 ? .semibold : .regular))
                        .lineLimit(1)
                    Spacer(minLength: 8)
                    Text(chat.updatedAt, format: .relative(presentation: .named, unitsStyle: .abbreviated))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                HStack(alignment: .top) {
                    Text(chat.preview.isEmpty ? "No messages yet" : Self.plain(chat.preview))
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                    Spacer(minLength: 8)
                    if chat.unread > 0 {
                        Text("\(chat.unread)")
                            .font(.caption2.bold())
                            .padding(.horizontal, 7).padding(.vertical, 2)
                            .background(Theme.accent, in: .capsule)
                            .foregroundStyle(.black)
                            .accessibilityLabel("\(chat.unread) unread")
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }

    /// Strips the noisiest Markdown punctuation for one-line previews.
    static func plain(_ s: String) -> String {
        s.replacingOccurrences(of: "```", with: "")
            .replacingOccurrences(of: "**", with: "")
            .replacingOccurrences(of: "`", with: "")
            .replacingOccurrences(of: "\n", with: " ")
    }
}
