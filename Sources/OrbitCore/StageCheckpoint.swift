import Foundation

public struct StageCheckpoint: Codable, Equatable, Sendable {
    public let rootPath: String
    public let identity: String
    public let eventID: UInt64
    public init(rootPath: String, identity: String, eventID: UInt64) {
        self.rootPath = rootPath; self.identity = identity; self.eventID = eventID
    }

    package func upgradesBootTraversal(from previous: StageCheckpoint) -> Bool {
        guard rootPath == "/", previous.rootPath == "/",
              var before = try? JSONSerialization.jsonObject(with: Data(previous.identity.utf8)) as? [String: Any],
              var after = try? JSONSerialization.jsonObject(with: Data(identity.utf8)) as? [String: Any],
              before["rootPath"] as? String == "/", after["rootPath"] as? String == "/",
              before["mode"] as? String == "host", after["mode"] as? String == "host",
              (before["bootTraversal"] == nil || before["bootTraversal"] as? Int == 1),
              after["bootTraversal"] as? Int == 2 else { return false }
        before.removeValue(forKey: "bootTraversal"); after.removeValue(forKey: "bootTraversal")
        return NSDictionary(dictionary: before).isEqual(to: after)
    }
}
