import Foundation

// Matches the scanner's depth-first ordering of individual directory entries.
enum HardLinkPathOrder {
    static func precedes(_ left: Data, _ right: Data) -> Bool {
        let a = String(decoding: left, as: UTF8.self).split(separator: "/").map(String.init)
        let b = String(decoding: right, as: UTF8.self).split(separator: "/").map(String.init)
        for (x, y) in zip(a, b) where x != y { return x < y }
        if a.count != b.count { return a.count < b.count }
        return left.lexicographicallyPrecedes(right)
    }
    static func path(_ parent: Data, _ name: Data) -> Data {
        var value = parent
        if value.last != 47 { value.append(47) }
        value.append(name)
        return value
    }
}
