import Foundation
import Testing
@testable import Hive

@Suite struct SSEParserTests {
    @Test func dispatchesOnBlankLine() {
        var p = SSEParser()
        #expect(p.feed(Data("event: chat.updated\ndata: {\"a\":1}\n".utf8)).isEmpty)
        let evs = p.feed(Data("\n".utf8))
        #expect(evs == [SSEEvent(event: "chat.updated", data: "{\"a\":1}", id: nil, retry: nil)])
    }

    @Test func multiLineDataJoinsWithNewline() {
        var p = SSEParser()
        let evs = p.feed(Data("data: one\ndata:two\ndata:  three\n\n".utf8))
        #expect(evs.first?.data == "one\ntwo\n three")
        #expect(evs.first?.event == "message")
    }

    @Test func handlesCRLFAndBareCR() {
        var p = SSEParser()
        let evs = p.feed(Data("event: a\r\ndata: 1\r\n\r\nevent: b\rdata: 2\r\r".utf8))
        #expect(evs.map(\.event) == ["a", "b"])
        #expect(evs.map(\.data) == ["1", "2"])
    }

    @Test func crlfSplitAcrossChunks() {
        var p = SSEParser()
        var evs = p.feed(Data("data: x\r".utf8))
        evs += p.feed(Data("\n\r".utf8))
        evs += p.feed(Data("\n".utf8))
        #expect(evs.count == 1)
        #expect(evs.first?.data == "x")
    }

    @Test func eventsSplitAtArbitraryBytes() {
        let stream = "id: 7\nevent: message.delta\ndata: {\"chatId\":\"c\",\"messageId\":\"m\",\"text\":\"héllo 🐝\"}\n\n: hb\n\nevent: turn.status\ndata: {}\n\n"
        let bytes = Array(stream.utf8)
        for split in 1..<bytes.count {
            var p = SSEParser()
            let evs = p.feed(Array(bytes[..<split])) + p.feed(Array(bytes[split...]))
            #expect(evs.count == 2, "split at \(split)")
            #expect(evs.first?.data.contains("🐝") == true)
        }
    }

    @Test func commentsAreIgnoredAndIdPersists() {
        var p = SSEParser()
        let evs = p.feed(Data(": heartbeat\n\nid: 41\nevent: a\ndata: x\n\nevent: b\ndata: y\n\n".utf8))
        #expect(evs.map(\.event) == ["a", "b"])
        #expect(evs.map(\.id) == ["41", "41"])
        #expect(p.lastEventId == "41")
    }

    @Test func eventWithoutDataIsNotDispatchedButIdUpdates() {
        var p = SSEParser()
        let evs = p.feed(Data("id: 5\nevent: ping\n\n".utf8))
        #expect(evs.isEmpty)
        #expect(p.lastEventId == "5")
    }

    @Test func retryFieldParsed() {
        var p = SSEParser()
        let evs = p.feed(Data("retry: 2500\ndata: x\n\nretry: nope\ndata: y\n\n".utf8))
        #expect(evs.first?.retry == 2500)
        #expect(evs.last?.retry == nil)
        #expect(p.retryMillis == 2500)
    }

    @Test func leadingBOMStripped() {
        var p = SSEParser()
        let evs = p.feed(Data("\u{FEFF}event: a\ndata: 1\n\n".utf8))
        #expect(evs.first?.event == "a")
    }

    @Test func fieldWithoutColon() {
        var p = SSEParser()
        let evs = p.feed(Data("data\n\n".utf8))
        #expect(evs.first?.data == "")
    }
}

@Suite struct ServerEventTests {
    @Test func decodesAllContractEvents() throws {
        let msg = #"{"id":"m1","chatId":"c1","author":"gpt-oss","role":"agent","agentId":"a1","body":"","createdAt":"2026-10-20T12:00:00Z","streaming":true}"#
        let chat = #"{"id":"c1","title":"Lab","updatedAt":"2026-10-20T12:00:00Z","preview":"p","unread":0}"#
        let prop = #"{"id":"p1","chatId":"c1","title":"T","body":"B","kind":"edit","status":"open","createdAt":"2026-10-20T12:00:00Z","author":"a"}"#

        func ev(_ name: String, _ data: String) throws -> ServerEvent {
            try ServerEvent.decode(SSEEvent(event: name, data: data, id: nil, retry: nil))
        }
        guard case let .messageCreated(m) = try ev("message.created", #"{"message":"# + msg + "}") else {
            Issue.record("created"); return
        }
        #expect(m.streaming && m.agentId == "a1")
        #expect(try ev("message.delta", #"{"chatId":"c1","messageId":"m1","text":"Hi"}"#)
                == .messageDelta(MessageDelta(chatId: "c1", messageId: "m1", text: "Hi")))
        guard case .messageDone = try ev("message.done", #"{"message":"# + msg + "}") else { Issue.record("done"); return }
        guard case let .chatUpdated(c) = try ev("chat.updated", #"{"chat":"# + chat + "}") else { Issue.record("chat"); return }
        #expect(c.title == "Lab")
        guard case let .proposalCreated(p) = try ev("proposal.created", #"{"proposal":"# + prop + "}") else {
            Issue.record("proposal"); return
        }
        #expect(p.diff == nil)
        #expect(try ev("turn.status", #"{"chatId":"c1","agentId":"a1","state":"running"}"#)
                == .turnStatus(TurnStatus(chatId: "c1", agentId: "a1", state: .running, detail: nil)))
        #expect(try ev("future.thing", "{}") == .unknown("future.thing"))
    }

    @Test func backoffGrowsAndCaps() {
        var b = Backoff(base: 1, cap: 8, jitter: 0)
        let delays = (0..<6).map { _ in b.next(random: 0) }
        #expect(delays == [1, 2, 4, 8, 8, 8])
        b.reset()
        #expect(b.next(random: 0) == 1)
        var j = Backoff(base: 10, cap: 100, jitter: 0.2)
        #expect(j.next(random: 1) == 12)
        #expect(j.next(random: -1) == 16)
    }
}

@Suite struct EventStreamTests {
    /// The stream yields events, reconnects after EOF, and resumes with Last-Event-ID.
    @Test func reconnectsWithLastEventID() async throws {
        let body = "id: 3\nevent: turn.status\ndata: {\"chatId\":\"c1\",\"agentId\":\"a1\",\"state\":\"queued\"}\n\n"
            + ": hb\n\n"
            + "id: 4\nevent: message.delta\ndata: {\"chatId\":\"c1\",\"messageId\":\"m1\",\"text\":\"ok\"}\n\n"
        let (client, token) = makeClient { req in
            #expect(req.url?.path == "/v1/events")
            #expect(req.value(forHTTPHeaderField: "Accept") == "text/event-stream")
            return StubProtocol.Reply(status: 200, headers: ["Content-Type": "text/event-stream"], body: Data(body.utf8))
        }
        let stream = EventStream(client: client, session: StubProtocol.session(),
                                 backoff: Backoff(base: 0.05, cap: 0.05, jitter: 0))
        var events: [ServerEvent] = []
        var connects = 0
        var sawDisconnect = false
        for await update in stream.updates() {
            switch update {
            case .connected: connects += 1
            case let .event(e): events.append(e)
            case .disconnected: sawDisconnect = true
            default: break
            }
            if connects == 2 { break }
        }
        #expect(sawDisconnect)
        #expect(events.count == 2)
        #expect(events.last == .messageDelta(MessageDelta(chatId: "c1", messageId: "m1", text: "ok")))
        let reqs = StubProtocol.requests(token: token)
        #expect(reqs.count >= 2)
        #expect(reqs[0].value(forHTTPHeaderField: "Last-Event-ID") == nil)
        #expect(reqs[1].value(forHTTPHeaderField: "Last-Event-ID") == "4")
    }

    @Test func stopsOnUnauthorized() async {
        let (client, _) = makeClient { _ in .json("{}", status: 401) }
        let stream = EventStream(client: client, session: StubProtocol.session(),
                                 backoff: Backoff(base: 0.01, cap: 0.01, jitter: 0))
        var updates: [StreamUpdate] = []
        for await u in stream.updates() { updates.append(u) }
        #expect(updates == [.connecting, .unauthorized])
    }
}
