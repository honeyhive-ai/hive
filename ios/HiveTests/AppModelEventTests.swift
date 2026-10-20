import Foundation
import Testing
@testable import Hive

/// Event folding in `AppModel` (found in the live end-to-end run).
@MainActor
@Suite(.serialized) struct AppModelEventTests {
    @Test func failedTurnClearsItselfButNotANewerStatus() async throws {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        model.errorTurnLinger = .milliseconds(50)
        let failed = TurnStatus(chatId: "c1", agentId: "a1", state: .error, detail: "boom")
        model.apply(.turnStatus(failed))
        #expect(model.activeTurns(chatId: "c1").map(\.state) == [.error])
        try await Task.sleep(for: .milliseconds(300))
        #expect(model.activeTurns(chatId: "c1").isEmpty, "a failed turn must not stick forever")

        // A retry that starts before the linger ends stays visible.
        model.apply(.turnStatus(failed))
        model.apply(.turnStatus(TurnStatus(chatId: "c1", agentId: "a1", state: .running, detail: nil)))
        try await Task.sleep(for: .milliseconds(300))
        #expect(model.activeTurns(chatId: "c1").map(\.state) == [.running])
    }

    @Test func replyFinishingInTheOpenChatMarksItRead() async throws {
        MockBackend.shared.reset()
        let model = AppModel(launch: LaunchOptions(mockServer: true))
        let api = try #require(model.client)
        let chatId = MockFixtures.labChatId
        #expect(try await api.chats().first { $0.id == chatId }?.unread == 1)
        model.chatPath = [chatId]
        let reply = Message(id: "done-1", chatId: chatId, author: "gpt-oss", role: .agent, agentId: "a-gptoss",
                            body: "done", createdAt: Date())
        model.apply(.messageDone(reply))
        var unread = -1
        for _ in 0..<40 {
            try await Task.sleep(for: .milliseconds(50))
            unread = try await api.chats().first { $0.id == chatId }?.unread ?? -1
            if unread == 0 { break }
        }
        #expect(unread == 0, "the server's read mark should advance for a reply watched live")
    }

    @Test func proposalAuthorIsShownAsSent() {
        func p(_ author: String) -> Proposal {
            Proposal(id: "p", chatId: "c", title: "t", body: "", kind: "fileDiff", diff: nil, status: .open,
                     createdAt: Date(), author: author)
        }
        #expect(p("@coder").authorLabel == "@coder" && p("@coder").isFromAgent)
        #expect(p("Zaz").authorLabel == "Zaz" && !p("Zaz").isFromAgent)
        #expect(p("").authorLabel == "Unknown")
    }
}
