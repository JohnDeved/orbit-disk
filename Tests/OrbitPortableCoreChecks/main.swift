import Foundation
import Dispatch

struct CheckFailure: Error, CustomStringConvertible {
    let description: String
}

func require(_ condition: @autoclosure () -> Bool, _ message: String,
             file: StaticString = #filePath, line: UInt = #line) throws {
    guard condition() else { throw CheckFailure(description: "\(file):\(line): \(message)") }
}

func fixtureURL(_ path: String) -> URL {
    // File-path initializers can normalize Unicode on macOS; these fixtures need both byte spellings.
    URL(string: "file://" + path.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed)!)!
}

func leaf(_ path: String, size: Int64 = 10, allocated: Int64 = 16,
          identity: String? = "device:inode") -> DiskNode {
    let url = fixtureURL(path)
    return DiskNode(url: url, name: url.lastPathComponent, isDirectory: false,
                    size: size, allocatedSize: allocated, fileCount: 1, fileIdentity: identity)
}

func folder(_ path: String, children: [DiskNode], state: NodeScanState = .complete) -> DiskNode {
    let url = fixtureURL(path)
    return DiskNode(url: url, name: url.lastPathComponent, isDirectory: true,
                    size: children.reduce(0) { $0 + $1.size },
                    allocatedSize: children.reduce(0) { $0 + $1.allocatedSize },
                    fileCount: children.reduce(0) { $0 + $1.fileCount }, children: children,
                    fileIdentity: "folder:\(path)", scanState: state)
}

func removal(_ node: DiskNode) -> ScanRemoval {
    ScanRemoval(path: node.id, identity: node.fileIdentity!)
}

func expectCancellation(_ operation: () throws -> Void) throws {
    do {
        try operation()
        throw CheckFailure(description: "Expected CancellationError")
    } catch is CancellationError {}
}

func checkNodeAndCheckpoint() throws {
    let node = DiskNode(url: URL(fileURLWithPath: "/portable/link"), name: "link",
                        isDirectory: false, isSymbolicLink: true, size: 3, allocatedSize: 0,
                        fileCount: 1, isAccessible: false, rawAllocatedSize: 128,
                        fileIdentity: "7:9", scanState: .pending)
    try require(node.rawAllocatedSize == 128 && node.allocatedSize == 0, "Raw hard-link allocation was lost")
    try require(node.isSymbolicLink && !node.isAccessible && !node.isScanComplete, "Node flags were lost")
    let root = folder("/portable", children: [node], state: .scanning)
    try require(root.rawAllocatedSize == 0 && root.size == 3 && root.fileCount == 1, "Directory totals differ")
    let result = ScanResult(root: root, elapsed: 1.5, skippedCount: 2, issues: ["denied"])
    try require(result.root.id == root.id && result.elapsed == 1.5 && result.skippedCount == 2
                && result.issues == ["denied"], "Scan result metadata differs")
    for eventID in [UInt64(0), 1, UInt64.max] {
        let checkpoint = StageCheckpoint(rootPath: "/portable/é/日本語", identity: "volume:1", eventID: eventID)
        let data = try JSONEncoder().encode(checkpoint)
        let decoded = try JSONDecoder().decode(StageCheckpoint.self, from: data)
        try require(decoded == checkpoint, "Checkpoint failed its JSON round trip")
    }
    print("PASS: node metadata and full-width checkpoint serialization")
}

func checkRemovalIdentityAndTotals() throws {
    let victim = leaf("/portable/group/victim", size: 10, allocated: 16, identity: "1:1")
    let retained = leaf("/portable/group/retained", size: 20, allocated: 32, identity: "1:2")
    let group = folder("/portable/group", children: [victim, retained], state: .scanning)
    let sibling = leaf("/portable/group-old", size: 5, allocated: 8, identity: "1:3")
    let root = folder("/portable", children: [group, sibling], state: .scanning)
    let source = [root, group, victim, retained, sibling]
    let projection = DiskRemovalProjection(removals: [removal(victim), removal(victim),
        ScanRemoval(path: retained.id, identity: "replaced"),
        ScanRemoval(path: "/portable/missing", identity: "1:9")], source: source)
    guard let projected = projection.apply(root), let projectedGroup = projection.apply(group) else {
        throw CheckFailure(description: "Valid ancestors disappeared")
    }
    try require(projected.size == 25 && projected.allocatedSize == 40 && projected.fileCount == 2,
                "Removal did not subtract ancestor totals exactly once")
    try require(projectedGroup.size == 20 && projectedGroup.children.map(\.id) == [retained.id],
                "Nested ancestor retained the removed child")
    try require(projected.scanState == .scanning && projected.fileIdentity == root.fileIdentity,
                "Projection changed retained metadata")
    try require(projection.apply(victim) == nil && projection.apply(retained)?.size == retained.size,
                "Identity mismatch removed a replacement")
    try require(projection.apply(sibling)?.size == sibling.size, "Sibling-prefix boundary was ignored")
    let noIdentity = leaf("/portable/unknown", identity: nil)
    try require(DiskRemovalProjection(removals: [ScanRemoval(path: noIdentity.id, identity: "missing")],
                                     source: [noIdentity]).apply(noIdentity) != nil,
                "An unidentified node matched a removal")
    let parentRemoval = DiskRemovalProjection(removals: [removal(group), removal(victim)], source: source)
    let parentProjected = parentRemoval.apply(root)
    try require(parentProjected?.size == 5 && parentProjected?.allocatedSize == 8 && parentProjected?.fileCount == 1,
                "Overlapping parent and child removals were counted twice")
    try require(parentRemoval.apply(retained) == nil && parentRemoval.apply(sibling) != nil,
                "Removed parent coverage crossed a component boundary")
    print("PASS: removal identities, ancestor totals, duplicate and overlapping removals")
}

func checkRemovalUnicodeAndBounds() throws {
    let composed = leaf("/portable/é/child", size: 13, allocated: 16, identity: "composed")
    let decomposed = leaf("/portable/e\u{301}/child", size: 29, allocated: 32, identity: "decomposed")
    let first = folder("/portable/é", children: [composed])
    let second = folder("/portable/e\u{301}", children: [decomposed])
    try require(composed.id == decomposed.id && Data(composed.id.utf8) != Data(decomposed.id.utf8),
                "Fixture must contain canonically equal, byte-distinct paths")
    let root = folder("/portable", children: [first, second])
    let projection = DiskRemovalProjection(removals: [removal(composed)], source: [composed, decomposed])
    try require(projection.apply(first)?.size == 0 && projection.apply(second)?.size == 29,
                "Equivalent Unicode spellings shared an ancestor delta")
    try require(projection.apply(composed) == nil && projection.apply(decomposed) != nil,
                "Removal matched a different path spelling")
    try require(projection.apply(root)?.size == 29, "Unicode ancestor total is wrong")
    for (size, allocated, count) in [(Int64(12), Int64(16), 1), (13, 15, 1), (13, 16, 0)] {
        let inconsistent = DiskNode(url: root.url, name: root.name, isDirectory: true,
                                    size: size, allocatedSize: allocated, fileCount: count)
        try require(projection.apply(inconsistent) == nil, "Projection allowed a negative total")
    }
    let wholeRoot = DiskRemovalProjection(removals: [removal(root)], source: [root])
    try require(wholeRoot.apply(root) == nil && wholeRoot.apply(decomposed) == nil,
                "Root removal did not cover descendants")
    print("PASS: byte-exact Unicode projection and inconsistent-total rejection")
}

func checkPathOrdering() throws {
    let paths = ["/b", "/a-b", "/a/b", "/a", "/a/a", "/", "/é", "/e\u{301}"]
    let bytes = paths.map { Data($0.utf8) }
    let ordered = bytes.sorted(by: HardLinkPathOrder.precedes)
    let expected = ["/", "/a", "/a/a", "/a/b", "/a-b", "/b", "/e\u{301}", "/é"]
    try require(ordered == expected.map { Data($0.utf8) }, "Depth-first path ordering or Unicode tie break differs")
    for a in bytes {
        try require(!HardLinkPathOrder.precedes(a, a), "Ordering is not irreflexive")
        for b in bytes {
            try require(!(HardLinkPathOrder.precedes(a, b) && HardLinkPathOrder.precedes(b, a)),
                        "Ordering is not asymmetric")
            for c in bytes where HardLinkPathOrder.precedes(a, b) && HardLinkPathOrder.precedes(b, c) {
                try require(HardLinkPathOrder.precedes(a, c), "Ordering is not transitive")
            }
        }
    }
    for parent in ["/", "/portable", "/portable/"] {
        let name = Data("e\u{301}".utf8)
        let expectedPath = parent.hasSuffix("/") ? parent + "e\u{301}" : parent + "/e\u{301}"
        try require(HardLinkPathOrder.path(Data(parent.utf8), name) == Data(expectedPath.utf8),
                    "Path composition changed bytes or separators")
    }
    print("PASS: depth-first hard-link ordering, byte ties, and path composition")
}

func checkCollectionMembership() throws {
    let empty = CollectionCoverage()
    try require(empty.state(for: "/") == .none && empty.state(for: [String]()) == .none, "Empty collection covers paths")
    let path = "/Users/Zoë/プロジェクト"
    let coverage = CollectionCoverage(paths: [path + "/", "/Elsewhere/one.bin"])
    try require(coverage.state(for: path) == .included && coverage.state(for: path + "/nested/file") == .included,
                "Collected folder did not cover descendants")
    try require(coverage.state(for: "/Users/Zoë") == .containsItems && coverage.state(for: "/") == .containsItems,
                "Collection ancestors were lost")
    try require(coverage.state(for: path + "-old/file") == .none, "Collection crossed a component boundary")
    try require(coverage.state(for: [path, "/Elsewhere/one.bin"]) == .included
                && coverage.state(for: [path, "/Elsewhere/two.bin"]) == .containsItems,
                "Group collection state differs")
    let all = CollectionCoverage(paths: ["/"])
    try require(all.state(for: "/") == .included && all.state(for: "/any/child") == .included, "Root coverage failed")
    let exact = CollectionCoverage(paths: ["/Unicode/é", "/K/one", "/\u{212A}/two"])
    try require(exact.state(for: "/Unicode/é/child") == .included
                && exact.state(for: "/Unicode/e\u{301}/child") == .none, "Collection merged Unicode spellings")
    try require(exact.state(for: "/K") == .containsItems && exact.state(for: "/\u{212A}") == .containsItems
                && exact.state(for: "/K/two") == .none && exact.state(for: "/\u{212A}/one") == .none,
                "Equivalent ancestor names merged")
    let members = ["/group/a", "/group/b", "/group/c", "/group/é", "/group/e\u{301}"]
    let membership = Set(members.map { Data($0.utf8) })
    let candidates = members + ["/group", "/", "/group/a/child", "/group-old/a", "/other"]
    for mask in 0..<(1 << candidates.count) {
        let collected = candidates.enumerated().compactMap { mask & (1 << $0.offset) == 0 ? nil : $0.element }
        let item = CollectionCoverage(paths: collected)
        let expected = item.state(for: members)
        for distinct in [true, false] {
            try require(item.state(forGroup: members, distinct: distinct, parent: "/group", contains: membership.contains) == expected,
                        "Optimized group coverage differs for subset \(mask)")
        }
    }
    print("PASS: collection boundaries and Unicode membership, 1,024 group combinations")
}

func checkCancellation() async throws {
    let token = ScanCancellation()
    let first = try token.commitIfNotCancelled { 42 }
    try require(first == 42, "Active commit failed")
    enum Expected: Error { case failure }
    do { try token.commitIfNotCancelled { throw Expected.failure } } catch Expected.failure {}
    let next = try token.commitIfNotCancelled { 43 }
    try require(next == 43, "Throwing commit did not release its lock")
    DispatchQueue.concurrentPerform(iterations: 128) { _ in token.cancel() }
    var invoked = false
    try expectCancellation { try token.commitIfNotCancelled { invoked = true } }
    try require(token.isCancelled && !invoked, "Cancelled commit executed its body")
    let victim = leaf("/portable/victim")
    let projection = DiskRemovalProjection(removals: [removal(victim)], source: [victim])
    try expectCancellation {
        _ = try DiskRemovalProjection(removals: [removal(victim)], source: [victim], cancellation: token)
    }
    try expectCancellation { _ = try projection.apply(victim, cancellation: token) }
    try await Task.detached {
        withUnsafeCurrentTask { $0?.cancel() }
        try expectCancellation {
            _ = try DiskRemovalProjection(removals: [removal(victim)], source: [victim], cancellation: ScanCancellation())
        }
        try expectCancellation { _ = try projection.apply(victim, cancellation: ScanCancellation()) }
    }.value
    print("PASS: cancellation tokens, commit lock recovery, and task cancellation")
}

@main struct PortableCoreChecks {
    static func main() async throws {
        try checkNodeAndCheckpoint()
        try checkRemovalIdentityAndTotals()
        try checkRemovalUnicodeAndBounds()
        try checkPathOrdering()
        try checkCollectionMembership()
        try await checkCancellation()
        try checkCompactNames()
        print("7 portable core check groups passed")
    }
}
