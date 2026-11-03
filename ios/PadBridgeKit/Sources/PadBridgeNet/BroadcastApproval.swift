import Foundation

/// Pairing for the broadcast extension, which has no UI and (without an App
/// Group) can't see the app's pairings: an unknown computer is remembered as
/// pending and the broadcast ends with a message showing its code. If the
/// user starts the broadcast again within `window`, that counts as approval
/// and the computer is paired; it gets in on its next retry.
public struct BroadcastApproval {
    public static let window: TimeInterval = 60

    private let defaults: UserDefaults
    private let key = "pendingBroadcastHost"

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Remembers `request` as the one computer awaiting approval.
    public func recordPending(_ request: PairingRequest, at date: Date = Date()) {
        defaults.set(["key": hexString(request.peerKey), "name": request.peerName, "time": date.timeIntervalSince1970],
                     forKey: key)
    }

    /// Call when a broadcast starts: pairs the pending computer if it asked
    /// within the window, and returns its name. Clears it either way.
    @discardableResult
    public func approvePending(into store: PairingStore, at date: Date = Date()) -> String? {
        defer { defaults.removeObject(forKey: key) }
        guard let pending = defaults.dictionary(forKey: key),
              let hex = pending["key"] as? String, let peerKey = Self.bytes(hex),
              let name = pending["name"] as? String, let time = pending["time"] as? TimeInterval,
              date.timeIntervalSince1970 - time <= Self.window, date.timeIntervalSince1970 >= time else { return nil }
        store.add(key: peerKey, name: name)
        return name
    }

    /// The message shown when the broadcast ends for an unknown computer.
    public static func message(for request: PairingRequest) -> String {
        let code = "\(request.code.prefix(3)) \(request.code.suffix(3))"
        return "「\(request.peerName)」要求鏡像這台 iPad。\n確認碼 \(code)\n\n電腦上顯示相同數字的話，請在 60 秒內重新開始廣播以允許；不認得就忽略。"
    }

    private static func bytes(_ hex: String) -> [UInt8]? {
        guard hex.count == 64 else { return nil }
        var out: [UInt8] = []
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index..<next], radix: 16) else { return nil }
            out.append(byte)
            index = next
        }
        return out
    }
}
