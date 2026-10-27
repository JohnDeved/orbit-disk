import Foundation

public struct StageCheckpoint: Codable, Equatable, Sendable {
    public let rootPath: String
    public let identity: String
    public let eventID: UInt64
    public init(rootPath: String, identity: String, eventID: UInt64) {
        self.rootPath = rootPath; self.identity = identity; self.eventID = eventID
    }
}
