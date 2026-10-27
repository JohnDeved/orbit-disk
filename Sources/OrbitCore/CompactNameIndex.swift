import Foundation
#if canImport(Darwin)
import Darwin
#else
import Glibc
#endif

// The database owns the records. This disposable index only narrows name searches.
package final class CompactNameIndex: @unchecked Sendable {
    package struct Matches {
        package let count: Int
        package let ids: [Int64]?
    }
    private static let blockSize = 128
    private let names: [UInt8]
    private let offsets: [UInt32]
    private let ids: [Int64]
    private let postings: [[UInt32]]
    package let byteCount: Int
    package var recordCount: Int { ids.count }
    package var highWaterID: Int64 { ids.last ?? 0 }

    package final class Builder {
        private var names: [UInt8] = []
        private var offsets: [UInt32] = [0]
        private var ids: [Int64] = []
        private var postings = [[UInt32]](repeating: [], count: 65_536)
        private var seen = [UInt32](repeating: .max, count: 65_536)
        private var postingCount = 0
        package let byteLimit: Int
        package init(byteLimit: Int = 96 * 1024 * 1024) { self.byteLimit = byteLimit }
        package var byteCount: Int {
            names.count + offsets.count * 4 + ids.count * 8 + postingCount * 4 + postings.count * 24
        }
        package func append(id: Int64, name: UnsafeBufferPointer<UInt8>) -> Bool {
            // Account for geometric array capacity growth before admitting another record.
            guard byteCount + name.count * 5 + 12 <= byteLimit / 2,
                  ids.count < Int(UInt32.max), names.count + name.count < Int(UInt32.max) else { return false }
            let block = UInt32(ids.count / CompactNameIndex.blockSize)
            if name.count >= 3 {
                for offset in 0..<(name.count - 2) {
                    let bucket = CompactNameIndex.hash(name[offset], name[offset + 1], name[offset + 2])
                    if seen[bucket] != block {
                        postings[bucket].append(block); seen[bucket] = block; postingCount += 1
                    }
                }
            }
            names.append(contentsOf: name); offsets.append(UInt32(names.count)); ids.append(id)
            return true
        }
        package func finish() -> CompactNameIndex {
            CompactNameIndex(names: names, offsets: offsets, ids: ids, postings: postings, byteCount: byteCount * 2)
        }
    }

    private init(names: [UInt8], offsets: [UInt32], ids: [Int64], postings: [[UInt32]], byteCount: Int) {
        self.names = names; self.offsets = offsets; self.ids = ids; self.postings = postings; self.byteCount = byteCount
    }
    private static func hash(_ a: UInt8, _ b: UInt8, _ c: UInt8) -> Int {
        Int((UInt32(a) &* 25_193 ^ UInt32(b) &* 4_051 ^ UInt32(c) &* 97) & 65_535)
    }

    package func matches(_ needle: String, candidateLimit: Int = 2_048, stopAfterCandidateLimit: Bool = false, check: () throws -> Void) throws -> Matches {
        try check()
        let query = Array(needle.utf8)
        guard !query.isEmpty, !ids.isEmpty else { return Matches(count: 0, ids: []) }
        var chosen: Int?
        if query.count >= 3 {
            for offset in 0..<(query.count - 2) {
                let bucket = Self.hash(query[offset], query[offset + 1], query[offset + 2])
                if postings[bucket].isEmpty { return Matches(count: 0, ids: []) }
                if chosen == nil || postings[bucket].count < postings[chosen!].count { chosen = bucket }
            }
        }
        var count = 0, candidates: [Int64] = []
        let blockCount = chosen.map { postings[$0].count } ?? ((ids.count + Self.blockSize - 1) / Self.blockSize)
        try names.withUnsafeBufferPointer { nameBytes in
            try query.withUnsafeBufferPointer { queryBytes in
                blocks: for position in 0..<blockCount {
                    if position & 31 == 0 { try check() }
                    let block = chosen.map { Int(postings[$0][position]) } ?? position
                    let start = block * Self.blockSize, end = min(ids.count, start + Self.blockSize)
                    for row in start..<end {
                        let offset = Int(offsets[row]), length = Int(offsets[row + 1]) - offset
                        if length >= query.count,
                           memmem(nameBytes.baseAddress! + offset, length, queryBytes.baseAddress!, query.count) != nil {
                            count += 1
                            if count <= candidateLimit { candidates.append(ids[row]) }
                            else if stopAfterCandidateLimit { break blocks }
                        }
                    }
                }
            }
        }
        try check()
        return Matches(count: count, ids: count <= candidateLimit ? candidates : nil)
    }
}
