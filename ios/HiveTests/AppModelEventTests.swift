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

    // MARK: Missed events (backgrounding, reconnects)

    private func waitFor(_ cond: @MainActor () -> Bool) async {
        for _ in 0..<60 where !cond() { try? await Task.sleep(for: .milliseconds(50)) }
    }

    @Test func notificationTapReloadsTheChatAlreadyOnScreen() async {
        MockBackend.shared.reset()
        let model = AppModel(launch: LaunchOptions(mockServer: true))
        let chatId = MockFixtures.labChatId
        model.chatPath = [chatId]
        model.messages[chatId] = [] // stale: the reply landed while backgrounded
        model.handle(url: URL(string: "hive://chat/\(chatId)")!)
        await waitFor { !(model.messages[chatId] ?? []).isEmpty }
        #expect(!(model.messages[chatId] ?? []).isEmpty, "the open chat must reload on a notification tap")
    }

    @Test func reconnectResyncsListsAndTheOpenChat() async {
        MockBackend.shared.reset()
        let model = AppModel(launch: LaunchOptions(mockServer: true))
        let chatId = MockFixtures.labChatId
        model.chatPath = [chatId]
        model.apply(StreamUpdate.connected)
        model.apply(StreamUpdate.disconnected(reason: "blip", retryIn: 1))
        model.chats = []
        model.messages[chatId] = []
        model.turns[chatId] = ["a-gptoss": TurnStatus(chatId: chatId, agentId: "a-gptoss", state: .running, detail: nil)]
        model.apply(StreamUpdate.connected)
        await waitFor { !model.chats.isEmpty && !(model.messages[chatId] ?? []).isEmpty && model.activeTurns(chatId: chatId).isEmpty }
        #expect(!model.chats.isEmpty, "a reconnect refetches the chat list")
        #expect(!(model.messages[chatId] ?? []).isEmpty, "and the open chat")
        #expect(model.activeTurns(chatId: chatId).isEmpty, "a running turn with no streaming reply is dropped")
    }

    @Test func reconcileKeepsTurnsWithAStreamingReply() {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        model.chatPath = ["c1"]
        model.messages["c1"] = [Message(id: "r", chatId: "c1", author: "coder", role: .agent, agentId: "a1",
                                        body: "wor", createdAt: Date(), streaming: true)]
        model.turns["c1"] = [
            "a1": TurnStatus(chatId: "c1", agentId: "a1", state: .running, detail: nil),
            "a2": TurnStatus(chatId: "c1", agentId: "a2", state: .queued, detail: nil),
        ]
        model.turns["c2"] = ["a1": TurnStatus(chatId: "c2", agentId: "a1", state: .running, detail: nil)]
        model.reconcileTurns()
        #expect(model.activeTurns(chatId: "c1").map(\.agentId) == ["a1"])
        #expect(model.activeTurns(chatId: "c2").isEmpty)
    }

    @Test func finishedReplyClearsItsAgentsTurn() {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        model.apply(.turnStatus(TurnStatus(chatId: "c1", agentId: "a1", state: .running, detail: nil)))
        model.apply(.messageDone(Message(id: "r", chatId: "c1", author: "coder", role: .agent, agentId: "a1",
                                         body: "done", createdAt: Date())))
        #expect(model.activeTurns(chatId: "c1").isEmpty, "a missed turn.status done must not strand the strip")
    }

    @Test func cursorIsKeptForTheNextStream() {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        model.apply(StreamUpdate.cursor("41"))
        #expect(model.lastEventId == "41")
    }

    @Test func settledProposalIsNotFiledInTheOpenInbox() {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        func p(_ id: String, _ status: ProposalStatus) -> Proposal {
            Proposal(id: id, chatId: "c", title: "t", body: "", kind: "fileDiff", diff: nil, status: status,
                     createdAt: Date(), author: "@coder")
        }
        model.apply(.proposalCreated(p("old", .applied)))
        model.apply(.proposalCreated(p("new", .open)))
        #expect(model.proposals.map(\.id) == ["new"])
    }

    @Test func pageMergeKeepsStreamedTextAheadOfTheCheckpoint() {
        let t = Date()
        func m(_ id: String, _ body: String, streaming: Bool, at: Date? = nil) -> Message {
            Message(id: id, chatId: "c", author: "coder", role: .agent, agentId: "a1", body: body,
                    createdAt: at ?? t, streaming: streaming)
        }
        let local = [m("r", "Hello, wor", streaming: true)]
        // Checkpoint behind the deltas: keep the longer local text.
        #expect(AppModel.merge(page: [m("r", "Hello", streaming: true)], local: local)[0].body == "Hello, wor")
        // Finished on the server: the page is authoritative.
        #expect(AppModel.merge(page: [m("r", "Hello, world.", streaming: false)], local: local)[0].body == "Hello, world.")
        // A diverging local copy never wins.
        #expect(AppModel.merge(page: [m("r", "Bye", streaming: true)], local: local)[0].body == "Bye")
        // A local message newer than the page is kept.
        let sent = m("u", "next", streaming: false, at: t.addingTimeInterval(5))
        #expect(AppModel.merge(page: [m("r", "Hello", streaming: true)], local: local + [sent]).map(\.id) == ["r", "u"])
    }
}
