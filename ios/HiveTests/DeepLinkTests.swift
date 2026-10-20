import Foundation
import Testing
@testable import Hive

@Suite struct DeepLinkTests {
    @Test func pairRoundTrip() throws {
        let server = URL(string: "http://100.64.0.9:8787")!
        let link = DeepLink.pairURL(server: server, token: "tok_abc-123")
        #expect(link.scheme == "hive")
        #expect(DeepLink.parse(link) == .pair(ServerConfig(baseURL: server, token: "tok_abc-123")))
    }

    @Test func pairWithUnpaddedBase64URL() throws {
        // base64url("http://100.64.0.7:8787") contains no padding when stripped.
        let encoded = DeepLink.base64URLEncode("http://100.64.0.7:8787")
        #expect(!encoded.contains("="))
        let url = URL(string: "hive://pair?url=\(encoded)&token=t0k")!
        guard case let .pair(cfg)? = DeepLink.parse(url) else { Issue.record("no pair"); return }
        #expect(cfg.baseURL.absoluteString == "http://100.64.0.7:8787")
        #expect(cfg.token == "t0k")
    }

    @Test func base64URLAlphabet() {
        // "??>" encodes to "Pz8+" in standard base64 -> "Pz8-" in base64url.
        #expect(DeepLink.base64URLEncode("??>") == "Pz8-")
        #expect(DeepLink.base64URLDecode("Pz8-") == "??>")
        #expect(DeepLink.base64URLDecode("Pz8_") != nil)
        #expect(DeepLink.base64URLDecode("A") == nil)
    }

    @Test func pairRejectsBadInput() {
        #expect(DeepLink.parse(URL(string: "hive://pair?token=x")!) == nil)
        let enc = DeepLink.base64URLEncode("http://h:1")
        #expect(DeepLink.parse(URL(string: "hive://pair?url=\(enc)")!) == nil)
        #expect(DeepLink.parse(URL(string: "hive://pair?url=\(enc)&token=")!) == nil)
        let ftp = DeepLink.base64URLEncode("ftp://h")
        #expect(DeepLink.parse(URL(string: "hive://pair?url=\(ftp)&token=x")!) == nil)
        #expect(DeepLink.parse(URL(string: "https://pair?url=\(enc)&token=x")!) == nil)
    }

    @Test func chatLinks() {
        #expect(DeepLink.parse(URL(string: "hive://chat/c-123")!) == .chat(id: "c-123"))
        #expect(DeepLink.parse(URL(string: "HIVE://chat/0190a1b2-c3d4")!) == .chat(id: "0190a1b2-c3d4"))
        #expect(DeepLink.parse(URL(string: "hive://chat/abc%20def")!) == .chat(id: "abc def"))
        #expect(DeepLink.parse(URL(string: "hive://chat/")!) == nil)
        #expect(DeepLink.parse(URL(string: "hive://chat")!) == nil)
        #expect(DeepLink.parse(URL(string: "hive://unknown/x")!) == nil)
    }

    @Test func pastedTextIsTrimmed() {
        let link = DeepLink.pairURL(server: URL(string: "http://h:8787")!, token: "t")
        #expect(DeepLink.parse(text: "  \(link.absoluteString)\n") != nil)
        #expect(DeepLink.parse(text: "not a link") == nil)
    }

    @Test func normalizeServerURL() {
        #expect(DeepLink.normalizeServerURL("100.1.2.3:8787")?.absoluteString == "http://100.1.2.3:8787")
        #expect(DeepLink.normalizeServerURL(" https://sff.tail.ts.net ")?.host() == "sff.tail.ts.net")
        #expect(DeepLink.normalizeServerURL("ftp://x") == nil)
        #expect(DeepLink.normalizeServerURL("") == nil)
    }
}

@Suite(.serialized) struct KeychainTests {
    let keychain = Keychain(service: "com.zazulam.hive.lab.tests.\(UUID().uuidString)")

    @Test func setGetUpdateDelete() throws {
        #expect(try keychain.data(for: "a") == nil)
        try keychain.set(Data("one".utf8), for: "a")
        #expect(try keychain.data(for: "a") == Data("one".utf8))
        try keychain.set(Data("two".utf8), for: "a")
        #expect(try keychain.data(for: "a") == Data("two".utf8))
        try keychain.delete("a")
        #expect(try keychain.data(for: "a") == nil)
        try keychain.delete("a") // idempotent
    }

    @Test func serverStoreRoundTrip() throws {
        let store = ServerStore(keychain: keychain)
        #expect(store.load() == nil)
        let cfg = ServerConfig(baseURL: URL(string: "http://100.64.0.1:8787")!, token: "secret")
        try store.save(cfg)
        #expect(store.load() == cfg)
        try store.forget()
        #expect(store.load() == nil)
    }

    @Test func accountsAreIsolated() throws {
        try keychain.set(Data("x".utf8), for: "one")
        try keychain.set(Data("y".utf8), for: "two")
        #expect(try keychain.data(for: "one") == Data("x".utf8))
        try keychain.delete("one")
        #expect(try keychain.data(for: "two") == Data("y".utf8))
        try keychain.delete("two")
    }
}
