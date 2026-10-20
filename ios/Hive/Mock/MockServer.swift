import Foundation

/// In-process fake `hive serve` used by the `-mockServer` launch argument
/// (and by tests). It is wired in as a `URLProtocol`, so the real API client
/// and SSE client run unchanged against it.
final class MockBackend: @unchecked Sendable {
    static let shared = MockBackend()
    static let baseURL = URL(string: "http://mock.hive.invalid:8787")!
    static let token = "mock-token"

    private let lock = NSLock()
    private let queue = DispatchQueue(label: "hive.mock.backend")
    private var chats: [Chat] = []
    private var messages: [String: [Message]] = [:]
    private var agents: [Agent] = []
    private var hosts: [Host] = []
    private var proposals: [Proposal] = []
    private var subscribers: [UUID: @Sendable (Data) -> Void] = [:]
    private var eventCounter = 0
    private var demoStarted = false
    var streamDemoOnConnect = false

    private var heartbeat: DispatchSourceTimer?

    init() {
        reset()
        let timer = DispatchSource.makeTimerSource(queue: queue)
        timer.schedule(deadline: .now() + 15, repeating: 15)
        timer.setEventHandler { [weak self] in
            guard let self else { return }
            let subs = self.lock.withLock { Array(self.subscribers.values) }
            let frame = Data(": heartbeat\n\n".utf8)
            subs.forEach { $0(frame) }
        }
        timer.resume()
        heartbeat = timer
    }

    func reset() {
        lock.withLock {
            let f = MockFixtures.make(now: Date())
            chats = f.chats
            messages = f.messages
            agents = f.agents
            hosts = f.hosts
            proposals = f.proposals
            demoStarted = false
        }
    }

    // MARK: HTTP

    struct Response {
        var status: Int
        var body: Data
    }

    func handle(_ request: URLRequest) -> Response {
        guard let url = request.url else { return json(400, ["error": "no url"]) }
        let method = request.httpMethod ?? "GET"
        let parts = url.path.split(separator: "/").map(String.init)
        let authorized = request.value(forHTTPHeaderField: "Authorization") == "Bearer \(Self.token)"
        let body = request.bodyData()

        if parts == ["v1", "health"] {
            var h: [String: Any] = ["ok": true, "version": "0.1.0-mock"]
            if authorized {
                h["workspace"] = "Home"
                h["deviceLabel"] = "sff (mock)"
            }
            return json(200, h)
        }
        guard authorized else { return json(401, ["error": "unauthorized"]) }

        switch (method, parts) {
        case ("GET", ["v1", "chats"]):
            let list = lock.withLock { chats.sorted { $0.updatedAt > $1.updatedAt } }
            return encode(ChatsEnvelope(chats: list))
        case ("POST", ["v1", "chats"]):
            struct Body: Decodable { var title: String? }
            let title = (try? JSONDecoder().decode(Body.self, from: body))?.title
            let chat = Chat(id: "c-\(UUID().uuidString.prefix(8).lowercased())",
                            title: (title?.isEmpty == false ? title! : "New chat"),
                            updatedAt: Date(), preview: "", unread: 0)
            lock.withLock {
                chats.append(chat)
                messages[chat.id] = []
            }
            broadcast("chat.updated", ChatEnvelope(chat: chat))
            return encode(ChatEnvelope(chat: chat))
        case ("GET", let p) where p.count == 4 && p[1] == "chats" && p[3] == "messages":
            let chatId = p[2]
            let comps = URLComponents(url: url, resolvingAgainstBaseURL: false)
            let after = comps?.queryItems?.first { $0.name == "after" }?.value
            let limit = comps?.queryItems?.first { $0.name == "limit" }?.value.flatMap(Int.init) ?? 100
            guard let all = lock.withLock({ messages[chatId] }) else { return json(404, ["error": "no such chat"]) }
            lock.withLock {
                if let i = chats.firstIndex(where: { $0.id == chatId }) { chats[i].unread = 0 }
            }
            let page: [Message]
            if let after, let idx = all.firstIndex(where: { $0.id == after }) {
                page = Array(all[(idx + 1)...].prefix(limit))
            } else {
                page = Array(all.suffix(limit))
            }
            return encode(MessagesEnvelope(messages: page))
        case ("POST", let p) where p.count == 4 && p[1] == "chats" && p[3] == "messages":
            struct Body: Decodable { var body: String }
            guard let text = (try? JSONDecoder().decode(Body.self, from: body))?.body,
                  !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            else { return json(400, ["error": "empty body"]) }
            let chatId = p[2]
            guard lock.withLock({ messages[chatId] != nil }) else { return json(404, ["error": "no such chat"]) }
            let msg = Message(id: "m-\(UUID().uuidString.prefix(8).lowercased())", chatId: chatId,
                              author: "you", role: .user, body: text, createdAt: Date())
            append(msg)
            broadcast("message.created", MessageEnvelope(message: msg))
            simulateReply(to: msg)
            return encode(MessageEnvelope(message: msg))
        case ("GET", ["v1", "agents"]):
            return encode(AgentsEnvelope(agents: lock.withLock { agents }))
        case ("GET", ["v1", "hosts"]):
            return encode(HostsEnvelope(hosts: lock.withLock { hosts }))
        case ("PUT", let p) where p.count == 4 && p[1] == "agents" && p[3] == "host":
            struct Body: Decodable { var hostId: String }
            guard let hostId = (try? JSONDecoder().decode(Body.self, from: body))?.hostId
            else { return json(400, ["error": "hostId required"]) }
            let result: Agent? = lock.withLock {
                guard let host = hosts.first(where: { $0.id == hostId }),
                      let i = agents.firstIndex(where: { $0.id == p[2] }) else { return nil }
                agents[i].hostId = host.id
                agents[i].hostLabel = host.label
                return agents[i]
            }
            guard let result else { return json(404, ["error": "unknown agent or host"]) }
            return encode(AgentEnvelope(agent: result))
        case ("GET", ["v1", "proposals"]):
            let all = URLComponents(url: url, resolvingAgainstBaseURL: false)?
                .queryItems?.first { $0.name == "status" }?.value == "all"
            let list = lock.withLock { proposals.filter { all || $0.status == .open } }
            return encode(ProposalsEnvelope(proposals: list))
        case ("POST", let p) where p.count == 4 && p[1] == "proposals" && (p[3] == "approve" || p[3] == "reject"):
            let result: Proposal? = lock.withLock {
                guard let i = proposals.firstIndex(where: { $0.id == p[2] }) else { return nil }
                proposals[i].status = p[3] == "approve" ? .approved : .rejected
                return proposals[i]
            }
            guard let result else { return json(404, ["error": "no such proposal"]) }
            return encode(ProposalEnvelope(proposal: result))
        case ("GET", ["v1", "lab", "status"]):
            return encode(MockFixtures.lab(now: Date()))
        default:
            return json(404, ["error": "not found"])
        }
    }

    // MARK: SSE

    func subscribe(_ send: @escaping @Sendable (Data) -> Void) -> UUID {
        let id = UUID()
        let startDemo: Bool = lock.withLock {
            subscribers[id] = send
            if streamDemoOnConnect && !demoStarted {
                demoStarted = true
                return true
            }
            return false
        }
        send(Data(": connected\n\n".utf8))
        if startDemo { runStreamingDemo() }
        return id
    }

    func unsubscribe(_ id: UUID) {
        _ = lock.withLock { subscribers.removeValue(forKey: id) }
    }

    private func broadcast<T: Encodable>(_ name: String, _ payload: T) {
        guard let data = try? HiveJSON.encoder.encode(payload) else { return }
        let (frame, subs): (Data, [@Sendable (Data) -> Void]) = lock.withLock {
            eventCounter += 1
            let text = "id: \(eventCounter)\nevent: \(name)\ndata: \(String(decoding: data, as: UTF8.self))\n\n"
            return (Data(text.utf8), Array(subscribers.values))
        }
        subs.forEach { $0(frame) }
    }

    private func append(_ msg: Message) {
        lock.withLock {
            messages[msg.chatId, default: []].append(msg)
            if let i = chats.firstIndex(where: { $0.id == msg.chatId }) {
                chats[i].updatedAt = msg.createdAt
                if !msg.body.isEmpty { chats[i].preview = String(msg.body.prefix(120)) }
            }
        }
    }

    private func update(_ msg: Message) {
        lock.withLock {
            if let i = messages[msg.chatId]?.firstIndex(where: { $0.id == msg.id }) {
                messages[msg.chatId]?[i] = msg
            }
            if let i = chats.firstIndex(where: { $0.id == msg.chatId }) {
                chats[i].updatedAt = Date()
                chats[i].preview = String(msg.body.prefix(120))
            }
        }
    }

    private func chat(_ id: String) -> Chat? { lock.withLock { chats.first { $0.id == id } } }

    /// Fakes an agent turn: queued -> running -> streamed reply -> done.
    private func simulateReply(to msg: Message) {
        let roster = lock.withLock { agents }
        let agent = roster.first { msg.body.localizedCaseInsensitiveContains("@\($0.name)") } ?? roster.first
        guard let agent else { return }
        let reply = "On it. Here's what I found for _\(msg.body.prefix(40))_:\n\n"
            + "- the gateway is healthy\n- `gpt-oss:20b` answered in **1.2 s**\n\n"
            + "```sh\ncurl -s http://gateway:8080/v1/models | jq '.data[].id'\n```"
        stream(reply, chatId: msg.chatId, agent: agent, interval: 0.06, startDelay: 0.4)
    }

    private func runStreamingDemo() {
        let roster = lock.withLock { agents }
        guard let agent = roster.first else { return }
        let reply = "Checking the SFF services now.\n\n"
            + "1. **gateway** responds on `:8080`, all three models loaded\n"
            + "2. **rdnamcp** answers tool calls; median latency 38 ms\n"
            + "3. Prometheus scrape targets are all `up`\n\n"
            + "I'll draft a proposal that raises the KV cache for `gpt-oss:20b` so long agent loops stop"
            + " re-evaluating the prompt. Give me a moment to diff the config and run the smoke tests"
            + " against the gateway before I open it for review."
        stream(reply, chatId: MockFixtures.labChatId, agent: agent, interval: 0.35, startDelay: 0.8)
    }

    private func stream(_ text: String, chatId: String, agent: Agent, interval: Double, startDelay: Double) {
        queue.asyncAfter(deadline: .now() + 0.1) { [self] in
            broadcast("turn.status", TurnStatus(chatId: chatId, agentId: agent.id, state: .queued, detail: nil))
        }
        queue.asyncAfter(deadline: .now() + startDelay) { [self] in
            broadcast("turn.status", TurnStatus(chatId: chatId, agentId: agent.id, state: .running, detail: nil))
            let reply = Message(id: "m-\(UUID().uuidString.prefix(8).lowercased())", chatId: chatId,
                                author: agent.name, role: .agent, agentId: agent.id, body: "",
                                createdAt: Date(), streaming: true)
            append(reply)
            broadcast("message.created", MessageEnvelope(message: reply))
            let chunks = Self.chunks(text)
            for (i, chunk) in chunks.enumerated() {
                queue.asyncAfter(deadline: .now() + interval * Double(i + 1)) { [self] in
                    var current = lock.withLock { messages[chatId]?.first { $0.id == reply.id } } ?? reply
                    current.body += chunk
                    update(current)
                    broadcast("message.delta", MessageDelta(chatId: chatId, messageId: reply.id, text: chunk))
                    if i == chunks.count - 1 {
                        current.streaming = false
                        update(current)
                        broadcast("message.done", MessageEnvelope(message: current))
                        broadcast("turn.status", TurnStatus(chatId: chatId, agentId: agent.id, state: .done, detail: nil))
                        if let chat = chat(chatId) { broadcast("chat.updated", ChatEnvelope(chat: chat)) }
                    }
                }
            }
        }
    }

    /// Splits text into word-ish chunks (keeps whitespace attached).
    static func chunks(_ text: String) -> [String] {
        var out: [String] = []
        var current = ""
        for ch in text {
            current.append(ch)
            if ch == " " || ch == "\n" {
                out.append(current)
                current = ""
            }
        }
        if !current.isEmpty { out.append(current) }
        return out
    }

    // MARK: Helpers

    private func json(_ status: Int, _ obj: [String: Any]) -> Response {
        Response(status: status, body: (try? JSONSerialization.data(withJSONObject: obj)) ?? Data())
    }

    private func encode<T: Encodable>(_ value: T) -> Response {
        do { return Response(status: 200, body: try HiveJSON.encoder.encode(value)) } catch {
            return json(500, ["error": "\(error)"])
        }
    }
}

extension URLRequest {
    /// Body bytes, including when URLSession converted `httpBody` into a stream.
    func bodyData() -> Data {
        if let httpBody { return httpBody }
        guard let stream = httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var data = Data()
        var buf = [UInt8](repeating: 0, count: 4096)
        while stream.hasBytesAvailable {
            let n = stream.read(&buf, maxLength: buf.count)
            if n <= 0 { break }
            data.append(buf, count: n)
        }
        return data
    }
}

/// Routes every request on a mock session to `MockBackend`.
final class MockServerProtocol: URLProtocol, @unchecked Sendable {
    nonisolated(unsafe) static var backend = MockBackend.shared
    private var subscription: UUID?
    private var backendRef: MockBackend?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let backend = Self.backend
        backendRef = backend
        guard let url = request.url else { return }
        if url.path == "/v1/events" {
            let authorized = request.value(forHTTPHeaderField: "Authorization") == "Bearer \(MockBackend.token)"
            let status = authorized ? 200 : 401
            let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1",
                                           headerFields: ["Content-Type": "text/event-stream"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            guard authorized else {
                client?.urlProtocolDidFinishLoading(self)
                return
            }
            subscription = backend.subscribe { [weak self] data in
                guard let self else { return }
                self.client?.urlProtocol(self, didLoad: data)
            }
            return
        }
        let result = backend.handle(request)
        let response = HTTPURLResponse(url: url, statusCode: result.status, httpVersion: "HTTP/1.1",
                                       headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: result.body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {
        if let subscription { backendRef?.unsubscribe(subscription) }
        subscription = nil
    }

    static func session() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockServerProtocol.self]
        config.timeoutIntervalForRequest = 600
        return URLSession(configuration: config)
    }
}
