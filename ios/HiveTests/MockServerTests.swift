import Foundation
import Testing
@testable import Hive

/// End-to-end: the real API + SSE clients against the in-process mock server.
@Suite(.serialized) struct MockServerTests {
    let session = MockServerProtocol.session()
    var client: APIClient {
        APIClient(config: ServerConfig(baseURL: MockBackend.baseURL, token: MockBackend.token), session: session)
    }

    @Test func listsFixtures() async throws {
        MockBackend.shared.reset()
        let chats = try await client.chats()
        #expect(chats.first?.id == MockFixtures.labChatId)
        #expect(zip(chats, chats.dropFirst()).allSatisfy { $0.updatedAt >= $1.updatedAt })
        #expect(try await client.agents().count == 4)
        #expect(try await client.hosts().contains { $0.isThis })
        #expect(try await client.proposals().count == 2)
        #expect(try await client.labStatus().workers.count == 3)
        let h = try await client.health()
        #expect(h.workspace == "Home")
    }

    @Test func rejectsWrongToken() async {
        let bad = APIClient(config: ServerConfig(baseURL: MockBackend.baseURL, token: "nope"), session: session)
        await #expect(throws: APIError.unauthorized) { try await bad.chats() }
        #expect((try? await bad.health())?.workspace == nil)
    }

    @Test func pagingWithAfter() async throws {
        MockBackend.shared.reset()
        let all = try await client.messages(chatId: MockFixtures.labChatId)
        #expect(all.map(\.id) == ["m1", "m2", "m3"])
        #expect(try await client.messages(chatId: MockFixtures.labChatId, after: "m1").map(\.id) == ["m2", "m3"])
        #expect(try await client.messages(chatId: MockFixtures.labChatId, limit: 1).map(\.id) == ["m3"])
    }

    @Test func sendingAMentionStreamsAReply() async throws {
        MockBackend.shared.reset()
        let c = client
        let stream = EventStream(client: c, session: session)
        let collector = Task {
            var seen: [ServerEvent] = []
            for await u in stream.updates() {
                if case let .event(e) = u {
                    seen.append(e)
                    if case .turnStatus(let t) = e, t.state == .done { break }
                }
                if case .connected = u {
                    _ = try? await c.sendMessage(chatId: "c-infra", body: "@gemma quick status?")
                }
            }
            return seen
        }
        let timeout = Task {
            try await Task.sleep(for: .seconds(20))
            collector.cancel()
        }
        let seen = await collector.value
        timeout.cancel()

        var body = ""
        var states: [TurnState] = []
        var done: Message?
        for e in seen {
            switch e {
            case let .messageDelta(d): body += d.text
            case let .turnStatus(t): states.append(t.state)
            case let .messageDone(m): done = m
            default: break
            }
        }
        #expect(states == [.queued, .running, .done])
        let final = try #require(done)
        #expect(final.agentId == "a-gemma")
        #expect(final.streaming == false)
        #expect(final.body == body)
        #expect(seen.contains { if case .messageCreated(let m) = $0 { m.role == .user } else { false } })
    }

    @Test func approveRemovesFromOpen() async throws {
        MockBackend.shared.reset()
        #expect(try await client.approve(proposalId: "p1").status == .approved)
        #expect(try await client.proposals().map(\.id) == ["p2"])
        #expect(try await client.proposals(includeClosed: true).count == 2)
        MockBackend.shared.reset()
    }

    @Test func moveAgentToHost() async throws {
        MockBackend.shared.reset()
        let a = try await client.setAgentHost(agentId: "a-lfm", hostId: "h-mac")
        #expect(a.hostId == "h-mac" && a.hostLabel == "macbook")
        await #expect(throws: APIError.notFound) { try await client.setAgentHost(agentId: "a-lfm", hostId: "zzz") }
        MockBackend.shared.reset()
    }
}

/// AppModel event folding.
@MainActor
@Suite struct AppModelTests {
    @Test func deltasAppendAndDoneFinalizes() {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        let t0 = Date()
        let chat = Chat(id: "c1", title: "Lab", updatedAt: t0, preview: "", unread: 0)
        model.apply(.chatUpdated(chat))
        let m = Message(id: "m1", chatId: "c1", author: "gpt-oss", role: .agent, agentId: "a1",
                        body: "", createdAt: t0.addingTimeInterval(1), streaming: true)
        model.apply(.turnStatus(TurnStatus(chatId: "c1", agentId: "a1", state: .running, detail: nil)))
        #expect(model.activeTurns(chatId: "c1").count == 1)
        model.apply(.messageCreated(m))
        model.apply(.messageDelta(MessageDelta(chatId: "c1", messageId: "m1", text: "Hel")))
        model.apply(.messageDelta(MessageDelta(chatId: "c1", messageId: "m1", text: "lo")))
        model.apply(.messageDelta(MessageDelta(chatId: "c1", messageId: "unknown", text: "x")))
        #expect(model.messages["c1"]?.first?.body == "Hello")
        var final = m
        final.body = "Hello!"
        final.streaming = false
        model.apply(.messageDone(final))
        #expect(model.messages["c1"]?.count == 1)
        #expect(model.messages["c1"]?.first?.body == "Hello!")
        #expect(model.messages["c1"]?.first?.streaming == false)
        model.apply(.turnStatus(TurnStatus(chatId: "c1", agentId: "a1", state: .done, detail: nil)))
        #expect(model.activeTurns(chatId: "c1").isEmpty)
        #expect(model.chats.first?.preview == "Hello!")
    }

    @Test func deepLinksRoute() {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        model.tab = .lab
        model.handle(url: URL(string: "hive://chat/c42")!)
        #expect(model.tab == .chats)
        #expect(model.chatPath == ["c42"])
        let link = DeepLink.pairURL(server: URL(string: "http://h:1")!, token: "t")
        model.handle(url: link)
        #expect(model.pendingPair?.token == "t")
        model.handle(url: URL(string: "hive://nope")!)
        #expect(model.banner != nil)
    }

    @Test func pairingAgainstMock() async throws {
        let model = AppModel(launch: LaunchOptions(mockServer: true, showPairing: true))
        #expect(!model.isPaired)
        await #expect(throws: APIError.unauthorized) {
            try await model.pair(ServerConfig(baseURL: MockBackend.baseURL, token: "wrong"))
        }
        #expect(!model.isPaired)
        try await model.pair(ServerConfig(baseURL: MockBackend.baseURL, token: MockBackend.token))
        #expect(model.isPaired)
        #expect(model.health?.deviceLabel != nil)
        model.forget()
        #expect(!model.isPaired)
    }
}
