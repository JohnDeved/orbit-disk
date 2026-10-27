import AppKit
import Foundation
import SwiftUI
import OrbitCore

struct HoverRefreshModelChecks {
    @MainActor static func run(_ base: URL) async throws {
        let preferences = UserDefaults(suiteName: "orbit-hover-refresh-\(UUID())")!
        let rootURL = base.appendingPathComponent("root")
        let count = 2_048
        func child(_ i: Int, parent: URL? = nil, name: String? = nil, size: Int64 = 1,
                   identity: String? = nil, useNil: Bool = false, descendants: [DiskNode] = []) -> DiskNode {
            let name = name ?? "item-\(i)"
            return DiskNode(url: (parent ?? rootURL).appendingPathComponent(name), name: name,
                            isDirectory: !descendants.isEmpty, size: size, allocatedSize: size, fileCount: 1,
                            children: descendants, fileIdentity: useNil ? nil : (identity ?? "identity-\(i)"))
        }
        func folder(_ children: [DiskNode], identity: String = "root-id") -> DiskNode {
            let size = children.reduce(Int64(0)) { $0 + $1.size }
            return DiskNode(url: rootURL, name: "root", isDirectory: true, size: size, allocatedSize: size,
                            fileCount: children.count, children: children, fileIdentity: identity)
        }
        func shares(_ lhs: [DiskNode], _ rhs: [DiskNode]) -> Bool {
            lhs.withUnsafeBufferPointer { l in rhs.withUnsafeBufferPointer { r in l.baseAddress == r.baseAddress && l.count == r.count } }
        }
        let old = (0..<count).map { child($0) }
        for test in ["same", "reorder", "identity", "nil", "unicode", "collision", "growth", "shrink", "transition", "exit", "sort", "root", "nested", "indirect", "filter", "mixed"] {
            var previous = old
            if test == "sort" { previous.reverse() }
            if test == "nil" { previous[0] = child(0, useNil: true) }
            if test == "unicode" { previous[0] = child(0, name: "K") }
            if test == "collision" { previous[0] = child(0, name: "K"); previous[1] = child(1, name: "K") }
            if test == "nested" { previous[0] = child(0, descendants: [child(9999, parent: rootURL.appendingPathComponent("item-0"))]) }
            if test == "indirect" { previous[0] = child(0, parent: rootURL.appendingPathComponent("elsewhere")) }
            let model = DiskModel(cacheDirectory: base.appendingPathComponent(test), preferences: preferences, volumeDiscovery: { [] })
            let start = folder(previous)
            try await model.testInstallColorFolder(start)
            func hover() -> PreparedMapHoverGroup {
                let layout = SunburstPreparedLayout(root: start, mapRevision: model.mapRevision,
                    colorIndices: model.childColorIndices, rootChildIDs: model.mapChildIDs, rootChildIDSet: model.mapChildIDSet)
                precondition(layout.segments.count == 1 && (test == "collision" || layout.segments[0].reusesRootIDs))
                let segment = layout.segments[0]
                let group = layout.hoverGroup(for: segment.id)!
                model.setMapHover(nodes: segment.members, groupID: segment.id, preparedGroup: group)
                return group
            }
            if test == "filter" { model.query = "item" }
            let oldGroup = hover()
            let oldIDs = model.hoveredMapNodes.map(\.id)
            var next = (0..<count).map { child($0, size: 2) }
            switch test {
            case "reorder": next.reverse()
            case "identity": next[0] = child(0, size: 2, identity: "replacement")
            case "nil": next[0] = child(0, size: 2, useNil: true)
            case "unicode": next[0] = child(0, name: "K", size: 2)
            case "collision": next[0] = child(0, name: "K", size: 2); next[1] = child(1, name: "K", size: 2)
            case "growth": next.append(child(count, size: 2))
            case "shrink": next.removeLast()
            case "nested": next[0] = child(0, size: 2, descendants: [child(9999, parent: rootURL.appendingPathComponent("item-0"))])
            case "indirect": next[0] = child(0, parent: rootURL.appendingPathComponent("elsewhere"), size: 2)
            case "mixed": next[0] = child(0, size: 100_000)
            case "sort": next.reverse()
            default: break
            }
            let newFolder = folder(next, identity: test == "root" ? "other-root" : "root-id")
            try await model.testPrepareDeferredColorFolder(newFolder)
            if test == "transition" { model.setMapHover(nodes: [previous[1]], groupID: nil) }
            if test == "exit" { model.setMapHover(nodes: nil, groupID: nil) }
            if test == "sort" { model.sortByName = true }
            precondition(model.testInstallDeferredColorFolder())
            switch test {
            case "same", "mixed", "sort":
                precondition(shares(model.hoveredMapNodes, model.current!.children), "fast path not used for \(test)")
                precondition(model.mapRelatedRowIDs == Set(oldIDs) && model.mapHoverDescription == "\(count) items in root")
                precondition(model.hoveredMapNodes[0].size == (test == "mixed" ? 100_000 : 2))
                if test == "sort" {
                    for _ in 0..<3_000 {
                        if !model.isSortingRows { break }
                        try await Task.sleep(nanoseconds: 2_000_000)
                    }
                    precondition(!model.isSortingRows && model.displayedItems.first?.node.name == "item-0"
                                 && model.mapRelatedRowIDs == Set(oldIDs), "name sorting lost the current hover")
                }
            case "exit":
                precondition(model.hoveredMapNodes.isEmpty && model.hoveredMapGroupID == nil && model.mapRelatedRowIDs.isEmpty,
                             "stale prepared hover reappeared after pointer exit")
            case "collision":
                precondition(model.hoveredMapNodes.isEmpty, "colliding siblings must retain the baseline identity rejection")
            case "identity", "shrink", "root":
                precondition(model.hoveredMapNodes.isEmpty && model.mapRelatedRowIDs.isEmpty, "stale hover survived \(test)")
            case "transition":
                precondition(model.hoveredMapNodes.count == 1 && model.hoveredMapNodes[0].id == previous[1].id && model.hoveredMapNodes[0].size == 2)
            default:
                precondition(!shares(model.hoveredMapNodes, model.current!.children), "expected conservative fallback for \(test)")
                precondition(model.hoveredMapNodes.map(\.id) == oldIDs && model.hoveredMapNodes.count == count)
                precondition(model.hoveredMapNodes.allSatisfy { $0.size == 2 })
                if test == "unicode" {
                    precondition(model.hoveredMapNodes[0].id.utf8.elementsEqual(next[0].id.utf8), "canonical equal IDs must use new bytes")
                }
            }
            if test == "same" {
                let revision = model.mapRevision
                model.setMapHover(nodes: [previous[0]], groupID: oldGroup.id, preparedGroup: oldGroup)
                precondition(model.mapRevision == revision && model.hoveredMapNodes.count == count,
                             "old map callback replaced the fresh hover")
            }
            model.cancelScan()
        }
        print("PASS: grouped hover refresh, identity, canonical bytes, reorder, membership, filter, and pointer transition")
    }
}
