import Foundation
import Testing
@testable import Hive

@Suite struct APIClientTests {
    @Test func chatsDecodeAndSendBearer() async throws {
        let (client, token) = makeClient { req in
            #expect(req.httpMethod == "GET")
            #expect(req.url?.path == "/v1/chats")
            return .json("""
            {"chats":[{"id":"c1","title":"Lab","updatedAt":"2026-10-20T12:00:00Z","preview":"hi","unread":2},
                      {"id":"c2","title":"Old","updatedAt":"2026-10-19T08:30:15.123456789Z","preview":"","unread":0}]}
            """)
        }
        let chats = try await client.chats()
        #expect(chats.map(\.id) == ["c1", "c2"])
        #expect(chats[0].unread == 2)
        #expect(chats[1].updatedAt < chats[0].updatedAt)
        let req = try #require(StubProtocol.requests(token: token).first)
        #expect(req.value(forHTTPHeaderField: "Authorization") == "Bearer \(token)")
    }

    @Test func messagesPassesAfterAndLimit() async throws {
        let (client, _) = makeClient { req in
            let items = URLComponents(url: req.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            #expect(req.url?.path == "/v1/chats/c%201/messages" || req.url?.path == "/v1/chats/c 1/messages")
            #expect(items.contains(URLQueryItem(name: "after", value: "m9")))
            #expect(items.contains(URLQueryItem(name: "limit", value: "50")))
            return .json("""
            {"messages":[{"id":"m10","chatId":"c 1","author":"gpt-oss","role":"agent","agentId":"a1",
              "body":"**hi**","createdAt":"2026-10-20T12:00:00.5Z","streaming":true},
             {"id":"m11","chatId":"c 1","author":"x","role":"weird","body":"sys","createdAt":"2026-10-20T12:00:01Z"}]}
            """)
        }
        let msgs = try await client.messages(chatId: "c 1", after: "m9", limit: 50)
        #expect(msgs.count == 2)
        #expect(msgs[0].role == .agent && msgs[0].role.isAgent)
        #expect(msgs[0].streaming)
        #expect(msgs[1].role == .system) // unknown roles degrade gracefully
        #expect(msgs[1].streaming == false)
    }

    @Test func messagesWithoutAfterOmitsQuery() async throws {
        let (client, _) = makeClient { req in
            #expect(req.url?.query == nil)
            return .json(#"{"messages":[]}"#)
        }
        #expect(try await client.messages(chatId: "c1").isEmpty)
    }

    @Test func sendMessagePostsJSONBody() async throws {
        let (client, _) = makeClient { req in
            #expect(req.httpMethod == "POST")
            #expect(req.value(forHTTPHeaderField: "Content-Type") == "application/json")
            #expect(jsonObject(req.httpBody)["body"] as? String == "@gpt-oss ping")
            return .json("""
            {"message":{"id":"m1","chatId":"c1","author":"me","role":"user","body":"@gpt-oss ping",
              "createdAt":"2026-10-20T12:00:00Z","streaming":false}}
            """)
        }
        let m = try await client.sendMessage(chatId: "c1", body: "@gpt-oss ping")
        #expect(m.id == "m1" && m.role == .user)
    }

    @Test func createChatSendsOptionalTitle() async throws {
        let (client, _) = makeClient { req in
            #expect(jsonObject(req.httpBody)["title"] as? String == "Plans")
            return .json(#"{"chat":{"id":"c9","title":"Plans","updatedAt":"2026-10-20T12:00:00Z","preview":"","unread":0}}"#)
        }
        #expect(try await client.createChat(title: "Plans").id == "c9")
    }

    @Test func unauthorizedMapsTo401Error() async {
        let (client, _) = makeClient { _ in .json(#"{"error":"bad token"}"#, status: 401) }
        await #expect(throws: APIError.unauthorized) { try await client.agents() }
    }

    @Test func serverErrorCarriesMessage() async {
        let (client, _) = makeClient { _ in .json(#"{"error":"relay offline"}"#, status: 502) }
        await #expect(throws: APIError.http(status: 502, message: "relay offline")) { try await client.hosts() }
    }

    @Test func badPayloadIsDecodingError() async {
        let (client, _) = makeClient { _ in .json(#"{"agents":[{"id":1}]}"#) }
        do {
            _ = try await client.agents()
            Issue.record("expected failure")
        } catch let APIError.decoding(detail) {
            #expect(!detail.isEmpty)
        } catch {
            Issue.record("wrong error \(error)")
        }
    }

    @Test func setAgentHostUsesPUT() async throws {
        let (client, _) = makeClient { req in
            #expect(req.httpMethod == "PUT")
            #expect(req.url?.path == "/v1/agents/a1/host")
            #expect(jsonObject(req.httpBody)["hostId"] as? String == "h-sff")
            return .json(#"{"agent":{"id":"a1","name":"gpt-oss","runtimeLabel":"Local","model":"gpt-oss:20b","hostId":"h-sff","hostLabel":"sff"}}"#)
        }
        let a = try await client.setAgentHost(agentId: "a1", hostId: "h-sff")
        #expect(a.hostLabel == "sff")
    }

    @Test func proposalsStatusQueryAndActions() async throws {
        let proposal = #"{"id":"p1","chatId":"c1","title":"T","body":"B","kind":"edit","diff":"+x","status":"STATUS","createdAt":"2026-10-20T12:00:00Z","author":"gpt-oss"}"#
        let (client, token) = makeClient { req in
            switch req.url?.path {
            case "/v1/proposals":
                return .json(#"{"proposals":["# + proposal.replacingOccurrences(of: "STATUS", with: "open") + "]}")
            case "/v1/proposals/p1/approve":
                return .json(#"{"proposal":"# + proposal.replacingOccurrences(of: "STATUS", with: "approved") + "}")
            default:
                return .json(#"{"proposal":"# + proposal.replacingOccurrences(of: "STATUS", with: "rejected") + "}")
            }
        }
        let open = try await client.proposals()
        #expect(open.first?.status == .open)
        #expect(open.first?.diff == "+x")
        #expect(try await client.approve(proposalId: "p1").status == .approved)
        #expect(try await client.reject(proposalId: "p1", reason: "  not now ").status == .rejected)
        _ = try await client.reject(proposalId: "p1", reason: "   ")

        let reqs = StubProtocol.requests(token: token)
        #expect(reqs[0].url?.query == "status=open")
        #expect(reqs[2].url?.path == "/v1/proposals/p1/reject")
        #expect(jsonObject(reqs[2].httpBody)["reason"] as? String == "not now")
        #expect(jsonObject(reqs[3].httpBody)["reason"] == nil) // blank reason omitted
    }

    @Test func labStatusToleratesMissingFields() async throws {
        let (client, _) = makeClient { _ in
            .json(#"{"workers":[{"name":"sff","up":true}],"temps":[],"updatedAt":"2026-10-20T12:00:00Z","costPerHour":0.05}"#)
        }
        let lab = try await client.labStatus()
        #expect(lab.powerWatts == nil)
        #expect(lab.costPerHour == 0.05)
        #expect(abs((lab.costPerMonth ?? 0) - 36.0) < 0.0001)
        #expect(lab.workers.first?.up == true)
    }

    @Test func healthWithoutAuthFields() async throws {
        let (client, _) = makeClient { _ in .json(#"{"ok":true,"version":"1.15.0"}"#) }
        let h = try await client.health()
        #expect(h.ok && h.version == "1.15.0" && h.workspace == nil)
    }

    @Test func baseURLPathPrefixIsPreserved() throws {
        let client = APIClient(config: ServerConfig(baseURL: URL(string: "http://host:1/hive/")!, token: "t"))
        let req = try client.request("GET", "v1/chats")
        #expect(req.url?.absoluteString == "http://host:1/hive/v1/chats")
    }

    @Test func transportErrorsAreWrapped() async {
        let client = APIClient(config: ServerConfig(baseURL: URL(string: "http://127.0.0.1:1")!, token: "t"))
        do {
            _ = try await client.chats()
            Issue.record("expected failure")
        } catch let APIError.transport(detail) {
            #expect(!detail.isEmpty)
        } catch {
            Issue.record("wrong error \(error)")
        }
    }

    @Test func datesWithVariousPrecisions() {
        #expect(HiveJSON.parseDate("2026-10-20T12:00:00Z") != nil)
        #expect(HiveJSON.parseDate("2026-10-20T12:00:00.1Z") != nil)
        #expect(HiveJSON.parseDate("2026-10-20T12:00:00.123456789Z") != nil)
        #expect(HiveJSON.parseDate("2026-10-20T12:00:00.123456+00:00") != nil)
        #expect(HiveJSON.parseDate("yesterday") == nil)
        let a = HiveJSON.parseDate("2026-10-20T12:00:00.250Z")!
        let b = HiveJSON.parseDate("2026-10-20T12:00:00.250999Z")!
        #expect(abs(a.timeIntervalSince(b)) < 0.001)
    }
}
