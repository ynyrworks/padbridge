import Foundation

/// The iPad's identity key and the hosts it trusts, shared between the app
/// (which pairs) and the broadcast extension (which only checks) through an
/// App Group container. Without the App Group the store falls back to the
/// process's own storage, and the extension can't see the app's pairings.
public final class PairingStore: @unchecked Sendable {
    public struct Host: Codable, Equatable, Identifiable, Sendable {
        /// Public key, hex.
        public let key: String
        public let name: String
        public let pairedAt: Date
        public var id: String { key }
    }

    private struct Contents: Codable {
        var privateKey: Data
        var hosts: [Host]
    }

    /// The App Group this build was signed with (re-signing tools may rename it).
    public static let defaultAppGroup = "group.io.github.padbridge"

    /// The store for this process; shared with the other target when the App Group works.
    public static let shared: PairingStore = {
        let group = appGroupFromProvisioningProfile() ?? defaultAppGroup
        if let dir = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: group) {
            return PairingStore(url: dir.appendingPathComponent("pairing.json"), isShared: true)
        }
        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return PairingStore(url: support.appendingPathComponent("pairing.json"), isShared: false)
    }()

    /// Whether the app and the broadcast extension see the same store.
    public let isShared: Bool
    public let url: URL
    private let lock = NSLock()
    private var contents: Contents

    public init(url: URL, isShared: Bool) {
        self.url = url
        self.isShared = isShared
        if let data = try? Data(contentsOf: url), let contents = try? JSONDecoder().decode(Contents.self, from: data),
           (try? DeviceIdentity(rawRepresentation: contents.privateKey)) != nil {
            self.contents = contents
        } else {
            contents = Contents(privateKey: DeviceIdentity().privateKey.rawRepresentation, hosts: [])
            try? save()
        }
    }

    public var identity: DeviceIdentity {
        lock.withLock { (try? DeviceIdentity(rawRepresentation: contents.privateKey)) ?? DeviceIdentity() }
    }

    public var hosts: [Host] {
        lock.withLock {
            reload()
            return contents.hosts
        }
    }

    public func isPaired(_ key: [UInt8]) -> Bool {
        let hex = hexString(key)
        return lock.withLock {
            // The other process may have paired or removed a host since.
            reload()
            return contents.hosts.contains { $0.key == hex }
        }
    }

    public func add(key: [UInt8], name: String) {
        let hex = hexString(key)
        lock.withLock {
            reload()
            contents.hosts.removeAll { $0.key == hex }
            contents.hosts.append(Host(key: hex, name: name, pairedAt: Date()))
            try? save()
        }
    }

    public func remove(key: String) {
        lock.withLock {
            reload()
            contents.hosts.removeAll { $0.key == key }
            try? save()
        }
    }

    /// Call with the lock held.
    private func reload() {
        guard let data = try? Data(contentsOf: url),
              let fresh = try? JSONDecoder().decode(Contents.self, from: data),
              fresh.privateKey == contents.privateKey else { return }
        contents = fresh
    }

    /// Call with the lock held (or from `init`).
    private func save() throws {
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        #if os(iOS)
        let options: Data.WritingOptions = [.atomic, .completeFileProtectionUntilFirstUserAuthentication]
        #else
        let options: Data.WritingOptions = [.atomic]
        #endif
        try JSONEncoder().encode(contents).write(to: url, options: options)
    }

    /// Reads `com.apple.security.application-groups` from this bundle's
    /// embedded provisioning profile (a CMS-wrapped plist).
    static func appGroupFromProvisioningProfile() -> String? {
        guard let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision"),
              let data = try? Data(contentsOf: url),
              let start = data.range(of: Data("<?xml".utf8)),
              let end = data.range(of: Data("</plist>".utf8), in: start.lowerBound..<data.endIndex) else { return nil }
        let plist = data[start.lowerBound..<end.upperBound]
        guard let profile = try? PropertyListSerialization.propertyList(from: plist, format: nil) as? [String: Any],
              let entitlements = profile["Entitlements"] as? [String: Any],
              let groups = entitlements["com.apple.security.application-groups"] as? [String] else { return nil }
        return groups.first
    }
}
