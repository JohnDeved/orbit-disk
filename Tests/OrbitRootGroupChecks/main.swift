import AppKit
import SwiftUI
import Foundation
import OrbitCore

@main struct Checks {
    static func verify(_ root: DiskNode, label: String, provideIDs: Bool = true) {
        let ids = root.children.map(\.id), unique = Set(ids)
        let ordered = provideIDs ? ids : nil
        let members = provideIDs ? unique : nil
        let expected = SunburstLayout.make(root: root, rootChildIDs: ordered)
        let actual = SunburstLayout.make(root: root, rootChildIDs: ordered, rootChildIDSet: members)
        if label == "saturated" || label == "unicode collision" || label == "duplicate" {
            precondition(actual.allSatisfy { !$0.reusesRootIDs }, "\(label) must use the baseline path")
        }
        precondition(expected.count == actual.count, "segment count \(label)")
        for (left, right) in zip(expected, actual) {
            precondition(left.id == right.id && left.node?.id == right.node?.id && left.name == right.name &&
                         left.size == right.size && left.start.bitPattern == right.start.bitPattern && left.end.bitPattern == right.end.bitPattern &&
                         left.depth == right.depth && left.branchIndex == right.branchIndex && left.scanState == right.scanState &&
                         left.groupParentID == right.groupParentID && left.preparedMemberIDs == right.preparedMemberIDs &&
                         left.members.map(\.id) == right.members.map(\.id) && left.members.map(\.fileIdentity) == right.members.map(\.fileIdentity), "layout differs \(label)")
            precondition(zip(left.members, right.members).allSatisfy { $0.id.utf8.elementsEqual($1.id.utf8) }, "member byte order differs \(label)")
        }
        let revision = UUID()
        let layout = SunburstPreparedLayout(root: root, mapRevision: revision, rootChildIDs: ordered, rootChildIDSet: members)
        var oldLookup: [String: Int] = [:]
        for (index, segment) in expected.enumerated() {
            oldLookup[segment.id] = index
            for id in segment.preparedMemberIDs ?? segment.members.map(\.id) { oldLookup[id] = index }
        }
        for (key, value) in oldLookup { precondition(layout.index(for: key) == value, "index differs \(label)") }
        precondition(layout.index(for: "/not-a-child") == nil)
        for index in expected.indices where expected[index].groupParentID != nil {
            let grouped = expected[index].preparedMemberIDs ?? expected[index].members.map(\.id)
            precondition(layout.groupMembersAreDistinct(at: index) == (Set(grouped).count == grouped.count))
            for id in grouped { precondition(layout.contains(groupMember: Data(id.utf8), at: index), "group membership differs") }
            precondition(layout.hoverGroup(for: expected[index].id)?.memberIDs == Set(grouped))
        }
        if !ids.isEmpty {
            let coverage = CollectionCoverage(paths: Array(ids.prefix(1)))
            let presentation = SunburstPresentationCache().presentation(root: root, mapRevision: revision, coverage: coverage,
                                                                         rootChildIDs: ordered, rootChildIDSet: members)
            let reference = SunburstPresentationCache().presentation(root: root, mapRevision: revision, coverage: coverage,
                                                                      rootChildIDs: ordered)
            for (index, segment) in expected.enumerated() {
                precondition(presentation.collectionStates[index] == reference.collectionStates[index], "collection state differs \(label): \(segment.id)")
            }
        }
    }

    static func main() {
        let base = URL(fileURLWithPath: "/tmp/orbit-root-group-parity")
        var state: UInt64 = 0x129482d
        func next() -> UInt64 { state = state &* 6364136223846793005 &+ 1; return state }
        func node(_ index: Int, _ size: Int64, name: String? = nil, children: [DiskNode] = []) -> DiskNode {
            let name = name ?? "file-\(index)"
            return DiskNode(url: base.appendingPathComponent(name), name: name, isDirectory: !children.isEmpty,
                            size: size, allocatedSize: size, fileCount: children.isEmpty ? 1 : children.count,
                            children: children, fileIdentity: "identity-\(index)",
                            scanState: index % 11 == 0 ? .pending : index % 7 == 0 ? .scanning : .complete)
        }
        func root(_ nodes: [DiskNode], _ size: Int64) -> DiskNode {
            DiskNode(url: base, name: "root", isDirectory: true, size: size, allocatedSize: size,
                     fileCount: nodes.count, children: nodes, fileIdentity: "root")
        }
        for sample in 0..<200 {
            let count = sample < 8 ? [0, 1, 71, 72, 1023, 1024, 1025, 1500][sample] : Int(next() % 1600)
            var nodes = (0..<count).map { i in node(i, sample % 5 == 0 ? Int64(next() % 1000 + 1) : 100) }
            if count > 1 && sample % 4 == 0 { nodes[count / 2] = node(count / 2, 0) }
            if count > 1 && sample % 11 == 0 { nodes[0] = node(0, -1) }
            if count > 1 && sample % 6 == 0 { nodes[0] = node(0, 1_000_000) }
            let total = nodes.reduce(Int64(0)) { max(0, $0 + max(0, $1.size)) }
            let tree = root(nodes, max(1, total))
            verify(tree, label: "random \(sample)")
            if sample % 20 == 0 { verify(tree, label: "no IDs \(sample)", provideIDs: false) }
        }
        let large = (0..<1_025).map { node($0, Int64.max) }
        verify(root(large, Int64.max), label: "saturated")
        var unicode = (0..<1_025).map { node($0, 1) }
        unicode[0] = node(0, 1, name: "K")
        unicode[1] = node(1, 1, name: "K")
        precondition(Set(unicode.map(\.id)).count != unicode.count)
        verify(root(unicode, 1_025), label: "unicode collision")
        var duplicates = (0..<1_025).map { node($0, 1) }
        duplicates[0] = node(0, 1, name: "duplicate")
        duplicates[1] = node(1, 1, name: "duplicate")
        verify(root(duplicates, 1_025), label: "duplicate")
        for size: Int64 in [2_227, 2_228, 2_229, 2_230] {
            var boundary = (0..<1_024).map { node($0, 1) }
            boundary[0] = node(0, size)
            verify(root(boundary, 1_000_000), label: "threshold \(size)")
        }
        let groupedDirectories = (0..<1_024).map { i in
            node(i, 1, name: "grouped-directory-\(i)", children: [node(10_000 + i, 1)])
        }
        verify(root(groupedDirectories, 1_024), label: "grouped directories")
        let allGrouped = root((0..<1_024).map { node($0, 1) }, 1_024)
        var withZero = allGrouped.children
        withZero[500] = node(500, 0)
        let zeroRoot = root(withZero, 1_023)
        let zeroIDs = zeroRoot.children.map(\.id)
        precondition(SunburstLayout.make(root: zeroRoot, rootChildIDs: zeroIDs, rootChildIDSet: Set(zeroIDs)).allSatisfy { !$0.reusesRootIDs })
        verify(zeroRoot, label: "zero fallback")
        let allIDs = allGrouped.children.map(\.id)
        let aliased = SunburstLayout.make(root: allGrouped, rootChildIDs: allIDs, rootChildIDSet: Set(allIDs))
        precondition(aliased.count == 1)
        let sharesNodes = allGrouped.children.withUnsafeBufferPointer { old in
            aliased[0].members.withUnsafeBufferPointer { old.baseAddress == $0.baseAddress }
        }
        let sharesIDs = allIDs.withUnsafeBufferPointer { old in
            aliased[0].preparedMemberIDs!.withUnsafeBufferPointer { old.baseAddress == $0.baseAddress }
        }
        precondition(sharesNodes && sharesIDs, "Whole-root fast path did not reuse the prepared arrays")
        let reordered = root(Array(allGrouped.children.reversed()), 1_024)
        verify(reordered, label: "same-count reordering")
        var replacedChildren = reordered.children
        replacedChildren[0] = node(10_000, 2, name: replacedChildren[0].name)
        let replaced = root(replacedChildren, 1_025)
        verify(replaced, label: "same-count changed identity and size")
        let cache = SunburstPresentationCache()
        let first = cache.presentation(root: allGrouped, mapRevision: UUID(), coverage: CollectionCoverage(),
                                       rootChildIDs: allIDs, rootChildIDSet: Set(allIDs))
        let newIDs = replaced.children.map(\.id)
        let next = cache.presentation(root: replaced, mapRevision: UUID(), coverage: CollectionCoverage(),
                                      rootChildIDs: newIDs, rootChildIDSet: Set(newIDs))
        precondition(first.layout.segments[0].members[0].id != next.layout.segments[0].members[0].id &&
                     next.layout.segments[0].members[0].fileIdentity == "identity-10000", "Same-count refresh reused stale nodes")
        let nested = (0..<50).map { i -> DiskNode in
            let leaves = (0..<50).map { j in node(i * 50 + j, 1, name: "nested-\(i)-\(j)") }
            return node(i, 50, name: "directory-\(i)", children: leaves)
        }
        verify(root(nested, 2_500), label: "nested")
        print("PASS: grouping, bit-exact geometry, membership, index and Unicode/zero/overflow fallback parity")
    }
}
