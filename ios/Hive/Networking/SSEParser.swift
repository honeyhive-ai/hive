import Foundation

/// One dispatched Server-Sent Event.
struct SSEEvent: Sendable, Equatable {
    var event: String
    var data: String
    var id: String?
    var retry: Int?
}

/// Incremental `text/event-stream` parser (WHATWG rules).
///
/// Bytes are fed as they arrive; complete events come back as soon as their
/// terminating blank line is seen. We parse raw bytes instead of using
/// `URLSession.AsyncBytes.lines` because that sequence swallows empty lines,
/// which are exactly what delimit SSE events.
struct SSEParser: Sendable {
    private var buffer: [UInt8] = []
    private var eventName = ""
    private var dataLines: [String] = []
    private var sawData = false
    private var pendingId: String?
    private var pendingRetry: Int?
    private var sawFirstLine = false
    /// Last `id:` seen; survives across events (the reconnect cursor).
    private(set) var lastEventId: String?
    /// Last `retry:` value in milliseconds.
    private(set) var retryMillis: Int?
    /// Whether the previous chunk ended with a bare CR (the LF may follow).
    private var pendingCR = false

    init() {}

    mutating func feed(_ data: Data) -> [SSEEvent] {
        feed(Array(data))
    }

    mutating func feed(_ bytes: [UInt8]) -> [SSEEvent] {
        var out: [SSEEvent] = []
        for byte in bytes {
            if let ev = feed(byte: byte) { out.append(ev) }
        }
        return out
    }

    /// Feed one byte (used when iterating `URLSession.AsyncBytes`).
    mutating func feed(byte: UInt8) -> SSEEvent? {
        if pendingCR {
            pendingCR = false
            if byte == 0x0A { return nil } // CRLF: the CR already ended the line
        }
        switch byte {
        case 0x0A:
            return processLine()
        case 0x0D:
            pendingCR = true
            return processLine()
        default:
            buffer.append(byte)
            return nil
        }
    }

    private mutating func processLine() -> SSEEvent? {
        var line = String(decoding: buffer, as: UTF8.self)
        buffer.removeAll(keepingCapacity: true)
        if !sawFirstLine {
            sawFirstLine = true
            if line.hasPrefix("\u{FEFF}") { line.removeFirst() }
        }
        if line.isEmpty { return dispatch() }
        if line.hasPrefix(":") { return nil } // comment / heartbeat

        let field: Substring
        var value: Substring
        if let colon = line.firstIndex(of: ":") {
            field = line[..<colon]
            value = line[line.index(after: colon)...]
            if value.hasPrefix(" ") { value = value.dropFirst() }
        } else {
            field = Substring(line)
            value = ""
        }
        switch field {
        case "event": eventName = String(value)
        case "data":
            dataLines.append(String(value))
            sawData = true
        case "id":
            if !value.contains("\0") { pendingId = String(value) }
        case "retry":
            if let ms = Int(value), ms >= 0 { pendingRetry = ms }
        default: break
        }
        return nil
    }

    private mutating func dispatch() -> SSEEvent? {
        defer {
            eventName = ""
            dataLines.removeAll()
            sawData = false
            pendingId = nil
            pendingRetry = nil
        }
        if let pendingId { lastEventId = pendingId.isEmpty ? nil : pendingId }
        if let pendingRetry { retryMillis = pendingRetry }
        guard sawData else { return nil }
        return SSEEvent(
            event: eventName.isEmpty ? "message" : eventName,
            data: dataLines.joined(separator: "\n"),
            id: lastEventId,
            retry: pendingRetry)
    }
}

/// Typed `hive serve` events.
enum ServerEvent: Sendable, Equatable {
    case messageCreated(Message)
    case messageDelta(MessageDelta)
    case messageDone(Message)
    case chatUpdated(Chat)
    case proposalCreated(Proposal)
    case turnStatus(TurnStatus)
    case unknown(String)

    static func decode(_ ev: SSEEvent) throws -> ServerEvent {
        let data = Data(ev.data.utf8)
        let d = HiveJSON.decoder
        struct MessageBox: Decodable { var message: Message }
        struct ChatBox: Decodable { var chat: Chat }
        struct ProposalBox: Decodable { var proposal: Proposal }
        switch ev.event {
        case "message.created": return .messageCreated(try d.decode(MessageBox.self, from: data).message)
        case "message.delta": return .messageDelta(try d.decode(MessageDelta.self, from: data))
        case "message.done": return .messageDone(try d.decode(MessageBox.self, from: data).message)
        case "chat.updated": return .chatUpdated(try d.decode(ChatBox.self, from: data).chat)
        case "proposal.created": return .proposalCreated(try d.decode(ProposalBox.self, from: data).proposal)
        case "turn.status": return .turnStatus(try d.decode(TurnStatus.self, from: data))
        default: return .unknown(ev.event)
        }
    }
}
