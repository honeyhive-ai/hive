import Foundation
import Observation
import SwiftUI

enum AppTab: String, Hashable, CaseIterable, Sendable {
    case chats, agents, review, lab, settings
}

enum StreamState: Equatable {
    case idle, connecting, live
    case offline(String)
}

/// Launch options (also `UserDefaults` argument-domain keys, e.g. `-tab lab`).
struct LaunchOptions {
    var mockServer = false
    var mockStreaming = false
    var showPairing = false
    var tab: AppTab?
    var openChat: String?
    var openProposal: String?

    static func current() -> LaunchOptions {
        let args = ProcessInfo.processInfo.arguments
        let d = UserDefaults.standard
        var o = LaunchOptions()
        o.mockServer = args.contains("-mockServer")
        o.mockStreaming = args.contains("-mockStreaming")
        o.showPairing = args.contains("-showPairing")
        o.tab = d.string(forKey: "tab").flatMap(AppTab.init(rawValue:))
        o.openChat = d.string(forKey: "openChat")
        o.openProposal = d.string(forKey: "openProposal")
        return o
    }
}

@MainActor
@Observable
final class AppModel {
    // Connection
    private(set) var config: ServerConfig?
    private(set) var client: APIClient?
    private(set) var health: Health?
    private(set) var streamState: StreamState = .idle
    let isMock: Bool

    // Data
    var chats: [Chat] = []
    var messages: [String: [Message]] = [:]
    var agents: [Agent] = []
    var hosts: [Host] = []
    var proposals: [Proposal] = []
    var lab: LabStatus?
    /// Active (queued/running/error) turns per chat, keyed by agent id.
    var turns: [String: [String: TurnStatus]] = [:]

    // Navigation
    var tab: AppTab = .chats
    var chatPath: [String] = []
    var reviewPath: [String] = []
    var pendingPair: ServerConfig?
    var banner: String?
    /// How long a failed turn stays in a chat's status strip.
    var errorTurnLinger: Duration = .seconds(12)

    private let store: ServerStore
    private let session: URLSession
    private var eventsTask: Task<Void, Never>?
    let launch: LaunchOptions

    init(launch: LaunchOptions = .current(), store: ServerStore = ServerStore()) {
        self.launch = launch
        self.store = store
        isMock = launch.mockServer
        if launch.mockServer {
            MockBackend.shared.streamDemoOnConnect = launch.mockStreaming
            session = MockServerProtocol.session()
            if !launch.showPairing {
                connect(ServerConfig(baseURL: MockBackend.baseURL, token: MockBackend.token))
            }
        } else {
            let cfg = URLSessionConfiguration.default
            cfg.waitsForConnectivity = false
            cfg.timeoutIntervalForResource = 7 * 24 * 3600 // SSE stays open
            session = URLSession(configuration: cfg)
            if let saved = store.load() { connect(saved) }
        }
        if let t = launch.tab { tab = t }
        if let c = launch.openChat { tab = .chats; chatPath = [c] }
        if let p = launch.openProposal { tab = .review; reviewPath = [p] }
    }

    var isPaired: Bool { config != nil }

    // MARK: Pairing

    /// Verifies the server (health, then an authorized call) without saving.
    func verify(_ candidate: ServerConfig) async throws -> Health {
        let probe = APIClient(config: candidate, session: session)
        let h = try await probe.health()
        guard h.ok else { throw APIError.http(status: 503, message: "server reports not ok") }
        _ = try await probe.agents() // proves the token
        return h
    }

    func pair(_ candidate: ServerConfig) async throws {
        let h = try await verify(candidate)
        if !isMock { try store.save(candidate) }
        connect(candidate)
        health = h
        pendingPair = nil
    }

    func forget() {
        if !isMock { try? store.forget() }
        eventsTask?.cancel()
        eventsTask = nil
        config = nil
        client = nil
        health = nil
        streamState = .idle
        chats = []
        messages = [:]
        agents = []
        hosts = []
        proposals = []
        lab = nil
        turns = [:]
        chatPath = []
        reviewPath = []
        tab = .chats
    }

    private func connect(_ cfg: ServerConfig) {
        eventsTask?.cancel()
        config = cfg
        client = APIClient(config: cfg, session: session)
        startEvents()
    }

    // MARK: Deep links

    func handle(url: URL) {
        switch DeepLink.parse(url) {
        case let .pair(cfg)?:
            pendingPair = cfg
        case let .chat(id)?:
            tab = .chats
            chatPath = [id]
        case nil:
            banner = "Unrecognised link: \(url.absoluteString)"
        }
    }

    // MARK: Events

    func startEvents() {
        guard let client else { return }
        eventsTask?.cancel()
        let stream = EventStream(client: client, session: session)
        eventsTask = Task { [weak self] in
            for await update in stream.updates() {
                guard let self else { return }
                self.apply(update)
            }
        }
    }

    /// Restart the stream when returning to the foreground.
    func resume() {
        guard isPaired else { return }
        if case .live = streamState { return }
        startEvents()
        Task { await refreshAll() }
    }

    func suspend() {
        eventsTask?.cancel()
        eventsTask = nil
        streamState = .idle
    }

    private func apply(_ update: StreamUpdate) {
        switch update {
        case .connecting:
            if streamState != .live { streamState = .connecting }
        case .connected:
            streamState = .live
        case let .disconnected(reason, retryIn):
            streamState = .offline("\(reason) Retrying in \(Int(retryIn.rounded()))s.")
        case .unauthorized:
            streamState = .offline("Token rejected. Re-pair this phone.")
        case let .event(ev):
            apply(ev)
        }
    }

    func apply(_ ev: ServerEvent) {
        switch ev {
        case let .messageCreated(m):
            upsert(m)
            bumpChat(for: m)
        case let .messageDone(m):
            upsert(m)
            bumpChat(for: m)
            // A reply that finished while its chat is on screen has been read:
            // re-fetch the page, which also advances the server's read mark —
            // otherwise the next chat.updated / chats refresh counts it unread.
            if chatPath.last == m.chatId, client != nil {
                Task { await self.loadMessages(chatId: m.chatId) }
            }
        case let .messageDelta(d):
            var list = messages[d.chatId] ?? []
            if let i = list.firstIndex(where: { $0.id == d.messageId }) {
                list[i].body += d.text
                list[i].streaming = true
                messages[d.chatId] = list
            }
        case let .chatUpdated(c):
            var c = c
            if chatPath.last == c.id { c.unread = 0 }
            if let i = chats.firstIndex(where: { $0.id == c.id }) { chats[i] = c } else { chats.append(c) }
            chats.sort { $0.updatedAt > $1.updatedAt }
        case let .proposalCreated(p):
            if let i = proposals.firstIndex(where: { $0.id == p.id }) { proposals[i] = p } else { proposals.insert(p, at: 0) }
        case let .turnStatus(t):
            var perChat = turns[t.chatId] ?? [:]
            switch t.state {
            case .done: perChat[t.agentId] = nil
            default: perChat[t.agentId] = t
            }
            turns[t.chatId] = perChat.isEmpty ? nil : perChat
            // A failure is terminal: show it for a while, then clear it (unless
            // a newer status for that agent replaced it in the meantime).
            if t.state == .error {
                let linger = errorTurnLinger
                Task { [weak self] in
                    try? await Task.sleep(for: linger)
                    self?.clearTurn(t)
                }
            }
        case .unknown:
            break
        }
    }

    /// Drop `t` from the turn strip if it is still the agent's latest status.
    func clearTurn(_ t: TurnStatus) {
        guard var perChat = turns[t.chatId], perChat[t.agentId] == t else { return }
        perChat[t.agentId] = nil
        turns[t.chatId] = perChat.isEmpty ? nil : perChat
    }

    private func upsert(_ m: Message) {
        var list = messages[m.chatId] ?? []
        if let i = list.firstIndex(where: { $0.id == m.id }) {
            list[i] = m
        } else {
            list.append(m)
            list.sort { $0.createdAt < $1.createdAt }
        }
        messages[m.chatId] = list
    }

    private func bumpChat(for m: Message) {
        guard let i = chats.firstIndex(where: { $0.id == m.chatId }) else { return }
        chats[i].updatedAt = max(chats[i].updatedAt, m.createdAt)
        if !m.body.isEmpty { chats[i].preview = String(m.body.prefix(140)) }
        chats.sort { $0.updatedAt > $1.updatedAt }
    }

    // MARK: Loading

    func refreshAll() async {
        async let a: Void = refreshChats()
        async let b: Void = refreshAgents()
        async let c: Void = refreshProposals()
        async let d: Void = refreshHealth()
        _ = await (a, b, c, d)
    }

    func refreshHealth() async {
        guard let client else { return }
        if let h = try? await client.health() { health = h }
    }

    func refreshChats() async {
        await run { client in
            let list = try await client.chats()
            self.chats = list.map { c in
                var c = c
                if self.chatPath.last == c.id { c.unread = 0 }
                return c
            }
        }
    }

    func loadMessages(chatId: String) async {
        await run { client in
            let page = try await client.messages(chatId: chatId)
            // Keep any locally-streamed text newer than the page.
            var merged = page
            for local in self.messages[chatId] ?? [] where !page.contains(where: { $0.id == local.id }) {
                if local.createdAt > (page.last?.createdAt ?? .distantPast) { merged.append(local) }
            }
            self.messages[chatId] = merged
            if let i = self.chats.firstIndex(where: { $0.id == chatId }) { self.chats[i].unread = 0 }
        }
    }

    func send(chatId: String, body: String) async -> Bool {
        let text = body.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return false }
        var ok = false
        await run { client in
            let m = try await client.sendMessage(chatId: chatId, body: text)
            self.upsert(m)
            self.bumpChat(for: m)
            ok = true
        }
        return ok
    }

    func createChat(title: String?) async -> Chat? {
        var made: Chat?
        await run { client in
            let c = try await client.createChat(title: title)
            if !self.chats.contains(where: { $0.id == c.id }) { self.chats.insert(c, at: 0) }
            self.messages[c.id] = self.messages[c.id] ?? []
            made = c
        }
        return made
    }

    func refreshAgents() async {
        await run { client in
            async let a = client.agents()
            async let h = client.hosts()
            let (agents, hosts) = try await (a, h)
            self.agents = agents
            self.hosts = hosts
        }
    }

    func sendAgent(_ agent: Agent, to host: Host) async {
        await run { client in
            let updated = try await client.setAgentHost(agentId: agent.id, hostId: host.id)
            if let i = self.agents.firstIndex(where: { $0.id == updated.id }) { self.agents[i] = updated }
        }
    }

    func refreshProposals() async {
        await run { client in self.proposals = try await client.proposals() }
    }

    func approve(_ p: Proposal) async -> Bool {
        var ok = false
        await run { client in
            let updated = try await client.approve(proposalId: p.id)
            self.replace(updated)
            ok = true
        }
        return ok
    }

    func reject(_ p: Proposal, reason: String?) async -> Bool {
        var ok = false
        await run { client in
            let updated = try await client.reject(proposalId: p.id, reason: reason)
            self.replace(updated)
            ok = true
        }
        return ok
    }

    private func replace(_ p: Proposal) {
        if p.status == .open {
            if let i = proposals.firstIndex(where: { $0.id == p.id }) { proposals[i] = p }
        } else {
            proposals.removeAll { $0.id == p.id }
        }
    }

    func refreshLab() async {
        await run { client in self.lab = try await client.labStatus() }
    }

    // MARK: Helpers

    func agent(id: String?) -> Agent? {
        guard let id else { return nil }
        return agents.first { $0.id == id }
    }

    func chat(id: String) -> Chat? { chats.first { $0.id == id } }

    func activeTurns(chatId: String) -> [TurnStatus] {
        (turns[chatId] ?? [:]).values.sorted { $0.agentId < $1.agentId }
    }

    private func run(_ body: (APIClient) async throws -> Void) async {
        guard let client else { return }
        do {
            try await body(client)
        } catch is CancellationError {
        } catch let e as URLError where e.code == .cancelled {
        } catch let e as APIError {
            if case .transport(let detail) = e, detail.lowercased().contains("cancelled") { return }
            banner = e.errorDescription
        } catch {
            banner = error.localizedDescription
        }
    }
}
