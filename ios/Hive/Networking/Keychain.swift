import Foundation
import Security

enum KeychainError: Error, Equatable {
    case unexpectedStatus(OSStatus)
}

/// Minimal generic-password Keychain wrapper (this device only, after first unlock).
struct Keychain: Sendable {
    let service: String

    init(service: String = "com.zazulam.hive.lab") {
        self.service = service
    }

    private func baseQuery(_ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }

    func set(_ data: Data, for account: String) throws {
        let query = baseQuery(account)
        let attrs: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        var status = SecItemUpdate(query as CFDictionary, attrs as CFDictionary)
        if status == errSecItemNotFound {
            status = SecItemAdd(query.merging(attrs) { $1 } as CFDictionary, nil)
        }
        guard status == errSecSuccess else { throw KeychainError.unexpectedStatus(status) }
    }

    func data(for account: String) throws -> Data? {
        var query = baseQuery(account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &out)
        switch status {
        case errSecSuccess: return out as? Data
        case errSecItemNotFound: return nil
        default: throw KeychainError.unexpectedStatus(status)
        }
    }

    func delete(_ account: String) throws {
        let status = SecItemDelete(baseQuery(account) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeychainError.unexpectedStatus(status)
        }
    }

    // MARK: Typed helpers

    func setCodable<T: Encodable>(_ value: T, for account: String) throws {
        try set(try JSONEncoder().encode(value), for: account)
    }

    func codable<T: Decodable>(_ type: T.Type, for account: String) throws -> T? {
        guard let data = try data(for: account) else { return nil }
        return try JSONDecoder().decode(T.self, from: data)
    }
}

/// Persists the paired server (URL + bearer token) in the Keychain.
struct ServerStore: Sendable {
    var keychain = Keychain()
    static let account = "server"

    func load() -> ServerConfig? {
        try? keychain.codable(ServerConfig.self, for: Self.account)
    }

    func save(_ config: ServerConfig) throws {
        try keychain.setCodable(config, for: Self.account)
    }

    func forget() throws {
        try keychain.delete(Self.account)
    }
}
