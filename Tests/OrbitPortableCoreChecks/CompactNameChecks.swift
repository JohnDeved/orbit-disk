import Foundation

func checkCompactNames() throws {
    let builder = CompactNameIndex.Builder()
    var names: [String] = []
    for row in 0..<8_400 {
        let suffix: String
        switch row % 11 {
        case 0: suffix = "été"
        case 1: suffix = "e\u{301}"
        case 2: suffix = "日本語"
        case 3: suffix = "👩🏽‍💻"
        case 4: suffix = "%_[quote]'"
        default: suffix = "repeated"
        }
        let name = "common-\(row)-\(suffix).dat"
        names.append(name)
        let accepted = Array(name.utf8).withUnsafeBufferPointer { builder.append(id: Int64(row * 3 + 7), name: $0) }
        try require(accepted, "Compact fixture exceeded its budget")
    }
    let index = builder.finish()
    let queries = ["common", ".dat", "127-", "128-", "4095-", "4096-", "8399-", "été", "e\u{301}",
                   "日本", "👩🏽‍💻", "%_[", "quote]'", "repeated", "not-present", "\0", "dat\0ignored", "a", "at"]
    for query in queries {
        let needle = Data(query.utf8)
        let expected = names.enumerated().compactMap { row, name -> Int64? in
            Data(name.utf8).range(of: needle) == nil ? nil : Int64(row * 3 + 7)
        }
        for limit in [0, 1, 50, 2_048, 10_000] {
            let matches = try index.matches(query, candidateLimit: limit, check: {})
            try require(matches.count == expected.count, "Compact name count differs for \(query.debugDescription)")
            if expected.count <= limit {
                try require(matches.ids == expected, "Compact names omitted or reordered candidate IDs")
            } else {
                try require(matches.ids == nil, "Broad name query exceeded its candidate budget")
            }
        }
    }
    var checkpoints = 0
    try expectCancellation {
        _ = try index.matches("common") {
            checkpoints += 1
            if checkpoints == 3 { throw CancellationError() }
        }
    }
    try require(checkpoints == 3, "Broad native matching did not check cancellation in flight")
    let tiny = CompactNameIndex.Builder(byteLimit: 1)
    let accepted = Array("file".utf8).withUnsafeBufferPointer { tiny.append(id: 1, name: $0) }
    try require(!accepted && tiny.finish().recordCount == 0, "Name-cache budget admitted a partial record")
    print("PASS: compact name index byte parity, block boundaries, candidate limits, budgets, and cancellation")
}
