import Foundation

/// Exponential reconnect backoff with jitter: 1s, 2s, 4s ... capped.
struct Backoff: Sendable {
    var base: Double = 1
    var cap: Double = 30
    var jitter: Double = 0.2
    private(set) var attempt = 0

    init(base: Double = 1, cap: Double = 30, jitter: Double = 0.2) {
        self.base = base
        self.cap = cap
        self.jitter = jitter
    }

    /// Delay in seconds before the next attempt; advances the attempt counter.
    mutating func next(random: Double = Double.random(in: -1...1)) -> Double {
        let raw = min(cap, base * pow(2, Double(attempt)))
        attempt += 1
        return max(0, raw * (1 + jitter * random))
    }

    mutating func reset() { attempt = 0 }
}

enum StreamUpdate: Sendable, Equatable {
    case connecting
    case connected
    case event(ServerEvent)
    /// Connection dropped; `retryIn` seconds until the next attempt.
    case disconnected(reason: String, retryIn: Double)
    case unauthorized
}

/// Foreground SSE subscription to `GET /v1/events` with reconnect, backoff
/// and `Last-Event-ID` resume.
final class EventStream: Sendable {
    private let client: APIClient
    private let session: URLSession
    private let initialBackoff: Backoff

    init(client: APIClient, session: URLSession, backoff: Backoff = Backoff()) {
        self.client = client
        self.session = session
        self.initialBackoff = backoff
    }

    /// Runs until the consumer stops iterating (or the task is cancelled).
    func updates() -> AsyncStream<StreamUpdate> {
        let client = self.client
        let session = self.session
        let initialBackoff = self.initialBackoff
        return AsyncStream { continuation in
            let task = Task {
                var backoff = initialBackoff
                var lastEventId: String?
                var serverRetry: Double?
                while !Task.isCancelled {
                    continuation.yield(.connecting)
                    var reason = "stream ended"
                    do {
                        var req = try client.request("GET", "v1/events")
                        req.setValue("text/event-stream", forHTTPHeaderField: "Accept")
                        req.setValue("no-cache", forHTTPHeaderField: "Cache-Control")
                        // Heartbeats arrive every 15 s; 45 s of silence means a dead link.
                        req.timeoutInterval = 45
                        if let lastEventId { req.setValue(lastEventId, forHTTPHeaderField: "Last-Event-ID") }
                        let (bytes, response) = try await session.bytes(for: req)
                        guard let http = response as? HTTPURLResponse else { throw APIError.transport("not HTTP") }
                        if http.statusCode == 401 || http.statusCode == 403 {
                            continuation.yield(.unauthorized)
                            continuation.finish()
                            return
                        }
                        guard (200..<300).contains(http.statusCode) else {
                            throw APIError.http(status: http.statusCode, message: "")
                        }
                        continuation.yield(.connected)
                        backoff.reset()
                        var parser = SSEParser()
                        for try await byte in bytes {
                            guard let ev = parser.feed(byte: byte) else { continue }
                            if let id = ev.id { lastEventId = id }
                            if let retry = ev.retry { serverRetry = Double(retry) / 1000 }
                            if let decoded = try? ServerEvent.decode(ev) {
                                continuation.yield(.event(decoded))
                            }
                        }
                    } catch is CancellationError {
                        break
                    } catch let error as URLError where error.code == .cancelled {
                        break
                    } catch let error as APIError {
                        reason = error.errorDescription ?? "error"
                    } catch {
                        reason = error.localizedDescription
                    }
                    if Task.isCancelled { break }
                    var delay = backoff.next()
                    if let serverRetry, backoff.attempt == 1 { delay = max(delay, serverRetry) }
                    continuation.yield(.disconnected(reason: reason, retryIn: delay))
                    try? await Task.sleep(for: .seconds(delay))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
