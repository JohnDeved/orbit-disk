public struct ScanRemoval: Sendable {
    public let path: String
    public let identity: String
    public init(path: String, identity: String) { self.path = path; self.identity = identity }
}
