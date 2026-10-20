import Foundation

/// Where the home host lives and how to authenticate to it.
struct ServerConfig: Codable, Sendable, Hashable {
    var baseURL: URL
    var token: String
}

enum APIError: Error, LocalizedError, Equatable {
    case unauthorized
    case notFound
    case http(status: Int, message: String)
    case decoding(String)
    case transport(String)
    case invalidURL

    var errorDescription: String? {
        switch self {
        case .unauthorized: "The server rejected the token. Re-pair this phone."
        case .notFound: "Not found on the server."
        case let .http(status, message): message.isEmpty ? "Server error (\(status))." : "Server error (\(status)): \(message)"
        case let .decoding(detail): "Unexpected response from the server: \(detail)"
        case let .transport(detail): "Can't reach the home host: \(detail)"
        case .invalidURL: "That server URL isn't valid."
        }
    }
}

/// One client per paired server; all calls follow the `hive serve` v1 contract.
actor APIClient {
    nonisolated let config: ServerConfig
    private let session: URLSession

    init(config: ServerConfig, session: URLSession = .shared) {
        self.config = config
        self.session = session
    }

    // MARK: Endpoints

    func health() async throws -> Health {
        try await send("GET", "v1/health")
    }

    func chats() async throws -> [Chat] {
        let env: ChatsEnvelope = try await send("GET", "v1/chats")
        return env.chats
    }

    func createChat(title: String?) async throws -> Chat {
        struct Body: Encodable { var title: String? }
        let env: ChatEnvelope = try await send("POST", "v1/chats", body: Body(title: title))
        return env.chat
    }

    func messages(chatId: String, after: String? = nil, limit: Int? = nil) async throws -> [Message] {
        var query: [URLQueryItem] = []
        if let after { query.append(URLQueryItem(name: "after", value: after)) }
        if let limit { query.append(URLQueryItem(name: "limit", value: String(limit))) }
        let env: MessagesEnvelope = try await send("GET", "v1/chats/\(Self.escape(chatId))/messages", query: query)
        return env.messages
    }

    func sendMessage(chatId: String, body: String) async throws -> Message {
        struct Body: Encodable { var body: String }
        let env: MessageEnvelope = try await send("POST", "v1/chats/\(Self.escape(chatId))/messages", body: Body(body: body))
        return env.message
    }

    func agents() async throws -> [Agent] {
        let env: AgentsEnvelope = try await send("GET", "v1/agents")
        return env.agents
    }

    func hosts() async throws -> [Host] {
        let env: HostsEnvelope = try await send("GET", "v1/hosts")
        return env.hosts
    }

    func setAgentHost(agentId: String, hostId: String) async throws -> Agent {
        struct Body: Encodable { var hostId: String }
        let env: AgentEnvelope = try await send("PUT", "v1/agents/\(Self.escape(agentId))/host", body: Body(hostId: hostId))
        return env.agent
    }

    func proposals(includeClosed: Bool = false) async throws -> [Proposal] {
        let env: ProposalsEnvelope = try await send(
            "GET", "v1/proposals", query: [URLQueryItem(name: "status", value: includeClosed ? "all" : "open")])
        return env.proposals
    }

    func approve(proposalId: String) async throws -> Proposal {
        struct Empty: Encodable {}
        let env: ProposalEnvelope = try await send("POST", "v1/proposals/\(Self.escape(proposalId))/approve", body: Empty())
        return env.proposal
    }

    func reject(proposalId: String, reason: String?) async throws -> Proposal {
        struct Body: Encodable { var reason: String? }
        let trimmed = reason?.trimmingCharacters(in: .whitespacesAndNewlines)
        let env: ProposalEnvelope = try await send(
            "POST", "v1/proposals/\(Self.escape(proposalId))/reject",
            body: Body(reason: (trimmed?.isEmpty ?? true) ? nil : trimmed))
        return env.proposal
    }

    func labStatus() async throws -> LabStatus {
        try await send("GET", "v1/lab/status")
    }

    // MARK: Plumbing

    /// Builds an authorized request for `path` (relative, no leading slash).
    nonisolated func request(_ method: String, _ path: String, query: [URLQueryItem] = []) throws -> URLRequest {
        guard var comps = URLComponents(url: config.baseURL, resolvingAgainstBaseURL: false) else {
            throw APIError.invalidURL
        }
        var basePath = comps.percentEncodedPath
        if basePath.hasSuffix("/") { basePath.removeLast() }
        comps.percentEncodedPath = basePath + "/" + path
        comps.queryItems = query.isEmpty ? nil : query
        guard let url = comps.url else { throw APIError.invalidURL }
        var req = URLRequest(url: url)
        req.httpMethod = method
        req.setValue("Bearer \(config.token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        req.timeoutInterval = 20
        return req
    }

    private func send<T: Decodable>(_ method: String, _ path: String, query: [URLQueryItem] = []) async throws -> T {
        try await perform(try request(method, path, query: query))
    }

    private func send<T: Decodable, B: Encodable>(_ method: String, _ path: String, body: B) async throws -> T {
        var req = try request(method, path)
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try HiveJSON.encoder.encode(body)
        return try await perform(req)
    }

    private func perform<T: Decodable>(_ req: URLRequest) async throws -> T {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: req)
        } catch let error as URLError {
            throw APIError.transport(Self.describe(error))
        } catch {
            throw APIError.transport(String(describing: error))
        }
        guard let http = response as? HTTPURLResponse else {
            throw APIError.transport("not an HTTP response")
        }
        try Self.check(status: http.statusCode, data: data)
        do {
            return try HiveJSON.decoder.decode(T.self, from: data)
        } catch {
            throw APIError.decoding(Self.describe(error))
        }
    }

    /// The URL error plus what iOS says underneath it (the failing URL, the
    /// POSIX/network code and the network path), so a failed connection says
    /// *why*: a local-network denial, a VPN route or a refused port.
    static func describe(_ error: URLError) -> String {
        var parts = ["\(error.localizedDescription) (\(error.code.rawValue))"]
        if let url = error.failingURL { parts.append(url.absoluteString) }
        let info = (error as NSError).userInfo
        if let under = info[NSUnderlyingErrorKey] as? NSError {
            parts.append("\(under.domain) \(under.code)")
        }
        if let path = info["_NSURLErrorNWPathKey"] { parts.append("path: \(path)") }
        return parts.joined(separator: " · ")
    }

    static func check(status: Int, data: Data) throws {
        switch status {
        case 200..<300: return
        // Only 401 means a bad token (re-pair). The server answers 403 when the
        // token is fine but the workspace refuses the action (e.g. a vote below
        // the proposal's role floor); that must not tell the user to re-pair.
        case 401: throw APIError.unauthorized
        case 404: throw APIError.notFound
        default:
            var message = String(decoding: data.prefix(300), as: UTF8.self)
            if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
               let err = (obj["error"] as? String) ?? (obj["message"] as? String) {
                message = err
            }
            throw APIError.http(status: status, message: message.trimmingCharacters(in: .whitespacesAndNewlines))
        }
    }

    private static func escape(_ segment: String) -> String {
        var allowed = CharacterSet.urlPathAllowed
        allowed.remove(charactersIn: "/?#")
        return segment.addingPercentEncoding(withAllowedCharacters: allowed) ?? segment
    }

    private static func describe(_ error: Error) -> String {
        switch error {
        case let DecodingError.keyNotFound(key, ctx):
            "missing \(key.stringValue) at \(ctx.codingPath.map(\.stringValue).joined(separator: "."))"
        case let DecodingError.typeMismatch(_, ctx), let DecodingError.valueNotFound(_, ctx),
             let DecodingError.dataCorrupted(ctx):
            "\(ctx.debugDescription) at \(ctx.codingPath.map(\.stringValue).joined(separator: "."))"
        default:
            String(describing: error)
        }
    }
}
