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

    /// Who it's from, ready to show: the server already writes an agent as
    /// `@name` and a person by display name, so never add another `@`.
    var authorLabel: String {
        let a = author.trimmingCharacters(in: .whitespaces)
        return a.isEmpty ? "Unknown" : a
    }

    /// Whether an agent's turn produced it (the server's `@name` form).
    var isFromAgent: Bool { author.hasPrefix("@") }
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

/// One hour of the solar pane's chart: average solar production and lab draw.
struct SolarPoint: Codable, Sendable, Hashable, Identifiable {
    var t: Date
    var solarWatts: Double?
    var labWatts: Double?
    var id: Date { t }
}

/// The lab's solar side, present only when the Enphase gateway reports in.
/// `gridWatts` is positive while importing and negative while exporting.
struct LabSolar: Codable, Sendable, Hashable {
    var up: Bool
    var productionWatts: Double?
    var consumptionWatts: Double?
    var gridWatts: Double?
    var houseSolarShare: Double?
    var labSolarWatts: Double?
    var producedKwh24h: Double?
    var houseKwh24h: Double?
    var labKwh24h: Double?
    var labSolarKwh24h: Double?
    var gridImportKwh24h: Double?
    var gridExportKwh24h: Double?
    /// Share of the lab's energy over 24 h that the panels covered (0...1).
    var labOffset24h: Double?
    /// What that solar energy is worth at the retail rate.
    var labSolarValue24h: Double?
    var history: [SolarPoint]

    init(up: Bool, productionWatts: Double? = nil, consumptionWatts: Double? = nil, gridWatts: Double? = nil,
         houseSolarShare: Double? = nil, labSolarWatts: Double? = nil, producedKwh24h: Double? = nil,
         houseKwh24h: Double? = nil, labKwh24h: Double? = nil, labSolarKwh24h: Double? = nil,
         gridImportKwh24h: Double? = nil, gridExportKwh24h: Double? = nil, labOffset24h: Double? = nil,
         labSolarValue24h: Double? = nil, history: [SolarPoint] = []) {
        self.up = up
        self.productionWatts = productionWatts
        self.consumptionWatts = consumptionWatts
        self.gridWatts = gridWatts
        self.houseSolarShare = houseSolarShare
        self.labSolarWatts = labSolarWatts
        self.producedKwh24h = producedKwh24h
        self.houseKwh24h = houseKwh24h
        self.labKwh24h = labKwh24h
        self.labSolarKwh24h = labSolarKwh24h
        self.gridImportKwh24h = gridImportKwh24h
        self.gridExportKwh24h = gridExportKwh24h
        self.labOffset24h = labOffset24h
        self.labSolarValue24h = labSolarValue24h
        self.history = history
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        up = try c.decodeIfPresent(Bool.self, forKey: .up) ?? false
        productionWatts = try c.decodeIfPresent(Double.self, forKey: .productionWatts)
        consumptionWatts = try c.decodeIfPresent(Double.self, forKey: .consumptionWatts)
        gridWatts = try c.decodeIfPresent(Double.self, forKey: .gridWatts)
        houseSolarShare = try c.decodeIfPresent(Double.self, forKey: .houseSolarShare)
        labSolarWatts = try c.decodeIfPresent(Double.self, forKey: .labSolarWatts)
        producedKwh24h = try c.decodeIfPresent(Double.self, forKey: .producedKwh24h)
        houseKwh24h = try c.decodeIfPresent(Double.self, forKey: .houseKwh24h)
        labKwh24h = try c.decodeIfPresent(Double.self, forKey: .labKwh24h)
        labSolarKwh24h = try c.decodeIfPresent(Double.self, forKey: .labSolarKwh24h)
        gridImportKwh24h = try c.decodeIfPresent(Double.self, forKey: .gridImportKwh24h)
        gridExportKwh24h = try c.decodeIfPresent(Double.self, forKey: .gridExportKwh24h)
        labOffset24h = try c.decodeIfPresent(Double.self, forKey: .labOffset24h)
        labSolarValue24h = try c.decodeIfPresent(Double.self, forKey: .labSolarValue24h)
        history = try c.decodeIfPresent([SolarPoint].self, forKey: .history) ?? []
    }
}

struct LabStatus: Codable, Sendable, Hashable {
    var workers: [LabWorker]
    var powerWatts: Double?
    var costPerHour: Double?
    var ratePerKwh: Double?
    var temps: [LabTemp]
    var solar: LabSolar?
    var updatedAt: Date

    /// 30-day month projection of the hourly cost.
    var costPerMonth: Double? { costPerHour.map { $0 * 24 * 30 } }

    init(workers: [LabWorker], powerWatts: Double?, costPerHour: Double?, ratePerKwh: Double?,
         temps: [LabTemp], solar: LabSolar? = nil, updatedAt: Date) {
        self.workers = workers
        self.powerWatts = powerWatts
        self.costPerHour = costPerHour
        self.ratePerKwh = ratePerKwh
        self.temps = temps
        self.solar = solar
        self.updatedAt = updatedAt
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        workers = try c.decodeIfPresent([LabWorker].self, forKey: .workers) ?? []
        powerWatts = try c.decodeIfPresent(Double.self, forKey: .powerWatts)
        costPerHour = try c.decodeIfPresent(Double.self, forKey: .costPerHour)
        ratePerKwh = try c.decodeIfPresent(Double.self, forKey: .ratePerKwh)
        temps = try c.decodeIfPresent([LabTemp].self, forKey: .temps) ?? []
        solar = try c.decodeIfPresent(LabSolar.self, forKey: .solar)
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
