import Foundation
@testable import Hive

/// URLProtocol stub. Each test registers a handler under its own bearer token,
/// so parallel tests never see each other's requests.
final class StubProtocol: URLProtocol, @unchecked Sendable {
    struct Reply: Sendable {
        var status: Int = 200
        var headers: [String: String] = ["Content-Type": "application/json"]
        var body: Data = Data()

        static func json(_ s: String, status: Int = 200) -> Reply {
            Reply(status: status, body: Data(s.utf8))
        }
    }

    typealias Handler = @Sendable (URLRequest) -> Reply

    private static let lock = NSLock()
    nonisolated(unsafe) private static var handlers: [String: Handler] = [:]
    nonisolated(unsafe) private static var log: [String: [URLRequest]] = [:]

    static func register(token: String, _ handler: @escaping Handler) {
        lock.withLock {
            handlers[token] = handler
            log[token] = []
        }
    }

    static func requests(token: String) -> [URLRequest] {
        lock.withLock { log[token] ?? [] }
    }

    static func session() -> URLSession {
        let c = URLSessionConfiguration.ephemeral
        c.protocolClasses = [StubProtocol.self]
        return URLSession(configuration: c)
    }

    private static func token(of request: URLRequest) -> String {
        let auth = request.value(forHTTPHeaderField: "Authorization") ?? ""
        return auth.hasPrefix("Bearer ") ? String(auth.dropFirst(7)) : auth
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        var req = request
        req.httpBody = request.bodyData()
        let token = Self.token(of: req)
        let handler: Handler? = Self.lock.withLock {
            Self.log[token, default: []].append(req)
            return Self.handlers[token]
        }
        let reply = handler?(req) ?? Reply(status: 401, body: Data(#"{"error":"unauthorized"}"#.utf8))
        let resp = HTTPURLResponse(url: req.url!, statusCode: reply.status, httpVersion: "HTTP/1.1",
                                   headerFields: reply.headers)!
        client?.urlProtocol(self, didReceive: resp, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: reply.body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

func makeClient(token: String = UUID().uuidString, base: String = "http://100.64.0.1:8787",
                _ handler: @escaping StubProtocol.Handler) -> (APIClient, String) {
    StubProtocol.register(token: token, handler)
    let client = APIClient(config: ServerConfig(baseURL: URL(string: base)!, token: token),
                           session: StubProtocol.session())
    return (client, token)
}

func jsonObject(_ data: Data?) -> [String: Any] {
    guard let data, let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [:] }
    return obj
}
