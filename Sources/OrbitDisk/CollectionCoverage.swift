import Foundation

enum CollectionState: Equatable, Sendable {
    case none
    case included
    case containsItems
}

struct CollectionCoverage: Equatable, Sendable {
    private let collected: Set<Data>
    private let ancestors: Set<Data>

    var isEmpty: Bool { collected.isEmpty }

    init(paths: [String] = []) {
        let paths = paths.map(Self.normalized).filter { !$0.isEmpty }
        collected = Set(paths.map { Data($0.utf8) })
        var ancestors = Set<Data>()
        for path in paths {
            var ancestor = Self.parent(of: path)
            while let current = ancestor {
                ancestors.insert(Data(current.utf8))
                ancestor = Self.parent(of: current)
            }
        }
        self.ancestors = ancestors
    }

    func state(for path: String) -> CollectionState {
        guard !isEmpty else { return .none }
        let path = Self.normalized(path)
        guard !path.isEmpty else { return .none }
        var candidate: String? = path
        while let current = candidate {
            if collected.contains(Data(current.utf8)) { return .included }
            candidate = Self.parent(of: current)
        }
        return ancestors.contains(Data(path.utf8)) ? .containsItems : .none
    }

    func state(for paths: [String]) -> CollectionState {
        guard !isEmpty, !paths.isEmpty else { return .none }
        var allIncluded = true
        var anyCollected = false
        for path in paths {
            let state = state(for: path)
            allIncluded = allIncluded && state == .included
            anyCollected = anyCollected || state != .none
            if anyCollected && !allIncluded { return .containsItems }
        }
        return allIncluded ? .included : anyCollected ? .containsItems : .none
    }

    private static func normalized(_ path: String) -> String {
        var path = path
        while path.count > 1 && path.hasSuffix("/") { path.removeLast() }
        return path
    }

    private static func parent(of path: String) -> String? {
        guard path != "/", let separator = path.lastIndex(of: "/") else { return nil }
        return separator == path.startIndex ? "/" : String(path[..<separator])
    }
}
