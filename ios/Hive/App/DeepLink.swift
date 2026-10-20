import Foundation

/// `hive://` URLs the app understands.
///
/// - `hive://pair?url=<base64url(serverURL)>&token=<token>`
/// - `hive://chat/<chatId>`
enum DeepLink: Equatable, Sendable {
    case pair(ServerConfig)
    case chat(id: String)

    static func parse(_ url: URL) -> DeepLink? {
        guard url.scheme?.lowercased() == "hive",
              let comps = URLComponents(url: url, resolvingAgainstBaseURL: false)
        else { return nil }
        switch comps.host?.lowercased() {
        case "pair":
            let items = comps.queryItems ?? []
            guard let encoded = items.first(where: { $0.name == "url" })?.value,
                  let token = items.first(where: { $0.name == "token" })?.value, !token.isEmpty,
                  let decoded = base64URLDecode(encoded),
                  let server = normalizeServerURL(decoded)
            else { return nil }
            return .pair(ServerConfig(baseURL: server, token: token))
        case "chat":
            let id = comps.path.split(separator: "/").first.map(String.init)?.removingPercentEncoding
            guard let id, !id.isEmpty else { return nil }
            return .chat(id: id)
        default:
            return nil
        }
    }

    /// Parses a pairing link from pasted text (tolerates surrounding whitespace).
    static func parse(text: String) -> DeepLink? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed) else { return nil }
        return parse(url)
    }

    static func base64URLDecode(_ s: String) -> String? {
        var b64 = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        b64 = b64.trimmingCharacters(in: CharacterSet(charactersIn: "="))
        let rem = b64.count % 4
        if rem == 1 { return nil }
        if rem > 0 { b64 += String(repeating: "=", count: 4 - rem) }
        guard let data = Data(base64Encoded: b64) else { return nil }
        return String(data: data, encoding: .utf8)
    }

    static func base64URLEncode(_ s: String) -> String {
        Data(s.utf8).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    /// Accepts `http(s)://host[:port][/prefix]`; a bare `host:port` gets `http://`.
    static func normalizeServerURL(_ raw: String) -> URL? {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if !s.contains("://") { s = "http://" + s }
        guard let url = URL(string: s), let scheme = url.scheme?.lowercased(),
              scheme == "http" || scheme == "https", let host = url.host(), !host.isEmpty
        else { return nil }
        return url
    }

    /// The link a server would print, for round-tripping in tests and mock mode.
    static func pairURL(server: URL, token: String) -> URL {
        var comps = URLComponents()
        comps.scheme = "hive"
        comps.host = "pair"
        comps.queryItems = [
            URLQueryItem(name: "url", value: base64URLEncode(server.absoluteString)),
            URLQueryItem(name: "token", value: token),
        ]
        return comps.url!
    }
}
