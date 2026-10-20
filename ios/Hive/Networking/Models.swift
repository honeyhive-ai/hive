import Foundation

// Wire models for the `hive serve` phone API (JSON, camelCase, ISO-8601 UTC).

struct Chat: Codable, Sendable, Identifiable, Hashable {
    var id: String
    var title: String
    var updatedAt: Date
    var preview: String
    var unread: Int

    init(id: String, title: String, updatedAt: Date, preview: String = "", unread: Int = 0) {
        self.id = id
        self.title = title
        self.updatedAt = updatedAt
        self.preview = preview
        self.unread = unread
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        updatedAt = try c.decode(Date.self, forKey: .updatedAt)
        preview = try c.decodeIfPresent(String.self, forKey: .preview) ?? ""
        unread = try c.decodeIfPresent(Int.self, forKey: .unread) ?? 0
    }
}

enum MessageRole: String, Codable, Sendable, Hashable {
    case user, agent, assistant, system

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = MessageRole(rawValue: raw) ?? .system
    }

    /// Agent and assistant replies render the same way.
    var isAgent: Bool { self == .agent || self == .assistant }
}

struct Message: Codable, Sendable, Identifiable, Hashable {
    var id: String
    var chatId: String
    var author: String
    var role: MessageRole
    var agentId: String?
    var body: String
    var createdAt: Date
    var streaming: Bool

    init(id: String, chatId: String, author: String, role: MessageRole, agentId: String? = nil,
         body: String, createdAt: Date, streaming: Bool = false) {
        self.id = id
        self.chatId = chatId
        self.author = author
        self.role = role
        self.agentId = agentId
        self.body = body
        self.createdAt = createdAt
        self.streaming = streaming
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        chatId = try c.decode(String.self, forKey: .chatId)
        author = try c.decodeIfPresent(String.self, forKey: .author) ?? ""
        role = try c.decode(MessageRole.self, forKey: .role)
        agentId = try c.decodeIfPresent(String.self, forKey: .agentId)
        body = try c.decodeIfPresent(String.self, forKey: .body) ?? ""
        createdAt = try c.decode(Date.self, forKey: .createdAt)
        streaming = try c.decodeIfPresent(Bool.self, forKey: .streaming) ?? false
    }
}

struct Agent: Codable, Sendable, Identifiable, Hashable {
    var id: String
    var name: String
    var runtimeLabel: String
    var model: String?
    var hostId: String?
    var hostLabel: String?
}

struct Host: Codable, Sendable, Identifiable, Hashable {
    var id: String
    var label: String
    var isThis: Bool
    var online: Bool
}

enum ProposalStatus: String, Codable, Sendable {
    case open, approved, rejected, applied, other

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = ProposalStatus(rawValue: raw) ?? .other
    }
}

struct Proposal: Codable, Sendable, Identifiable, Hashable {
    var id: String
    var chatId: String
    var title: String
    var body: String
    var kind: String
    var diff: String?
    var status: ProposalStatus
    var createdAt: Date
    var author: String
}

struct LabWorker: Codable, Sendable, Hashable, Identifiable {
    var name: String
    var up: Bool
    var id: String { name }
}

struct LabTemp: Codable, Sendable, Hashable, Identifiable {
    var name: String
    var celsius: Double
    var id: String { name }
}

struct LabStatus: Codable, Sendable, Hashable {
    var workers: [LabWorker]
    var powerWatts: Double?
    var costPerHour: Double?
    var ratePerKwh: Double?
    var temps: [LabTemp]
    var updatedAt: Date

    /// 30-day month projection of the hourly cost.
    var costPerMonth: Double? { costPerHour.map { $0 * 24 * 30 } }

    init(workers: [LabWorker], powerWatts: Double?, costPerHour: Double?, ratePerKwh: Double?,
         temps: [LabTemp], updatedAt: Date) {
        self.workers = workers
        self.powerWatts = powerWatts
        self.costPerHour = costPerHour
        self.ratePerKwh = ratePerKwh
        self.temps = temps
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        workers = try c.decodeIfPresent([LabWorker].self, forKey: .workers) ?? []
        powerWatts = try c.decodeIfPresent(Double.self, forKey: .powerWatts)
        costPerHour = try c.decodeIfPresent(Double.self, forKey: .costPerHour)
        ratePerKwh = try c.decodeIfPresent(Double.self, forKey: .ratePerKwh)
        temps = try c.decodeIfPresent([LabTemp].self, forKey: .temps) ?? []
        updatedAt = try c.decode(Date.self, forKey: .updatedAt)
    }
}

struct Health: Codable, Sendable, Hashable {
    var ok: Bool
    var version: String
    var workspace: String?
    var deviceLabel: String?
}

enum TurnState: String, Codable, Sendable {
    case queued, running, done, error
}

struct TurnStatus: Codable, Sendable, Hashable {
    var chatId: String
    var agentId: String
    var state: TurnState
    var detail: String?
}

// MARK: - Envelopes

struct ChatsEnvelope: Codable, Sendable { var chats: [Chat] }
struct ChatEnvelope: Codable, Sendable { var chat: Chat }
struct MessagesEnvelope: Codable, Sendable { var messages: [Message] }
struct MessageEnvelope: Codable, Sendable { var message: Message }
struct AgentsEnvelope: Codable, Sendable { var agents: [Agent] }
struct AgentEnvelope: Codable, Sendable { var agent: Agent }
struct HostsEnvelope: Codable, Sendable { var hosts: [Host] }
struct ProposalsEnvelope: Codable, Sendable { var proposals: [Proposal] }
struct ProposalEnvelope: Codable, Sendable { var proposal: Proposal }

struct MessageDelta: Codable, Sendable, Hashable {
    var chatId: String
    var messageId: String
    var text: String
}

// MARK: - JSON coding

enum HiveJSON {
    static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .custom { decoder in
            let c = try decoder.singleValueContainer()
            let raw = try c.decode(String.self)
            if let date = parseDate(raw) { return date }
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "Bad ISO-8601 date: \(raw)")
        }
        return d
    }()

    static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .custom { date, encoder in
            var c = encoder.singleValueContainer()
            try c.encode(formatDate(date))
        }
        e.outputFormatting = [.sortedKeys]
        return e
    }()

    /// Accepts ISO-8601 with or without fractional seconds (any precision).
    static func parseDate(_ raw: String) -> Date? {
        if let d = try? Date(raw, strategy: .iso8601) { return d }
        let frac = Date.ISO8601FormatStyle(includingFractionalSeconds: true)
        if let d = try? Date(raw, strategy: frac) { return d }
        // Normalise >3 fractional digits (e.g. RFC 3339 nanoseconds from Rust).
        if let dot = raw.firstIndex(of: "."),
           let end = raw[dot...].firstIndex(where: { !$0.isNumber && $0 != "." }) {
            let digits = raw[raw.index(after: dot)..<end]
            let trimmed = raw[..<dot] + "." + digits.prefix(3).padding(toLength: 3, withPad: "0", startingAt: 0) + raw[end...]
            return try? Date(String(trimmed), strategy: frac)
        }
        return nil
    }

    static func formatDate(_ date: Date) -> String {
        date.formatted(Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }
}
