import Foundation
import OrbitCore

let useBulk = !CommandLine.arguments.contains("--posix")
let arguments = CommandLine.arguments.dropFirst().filter { $0 != "--posix" }
guard let path = arguments.first else {
    fputs("Usage: OrbitScan [--posix] <folder>\n", stderr)
    exit(2)
}

do {
    let result = try DiskScanner(useBulkEnumeration: useBulk).scan(URL(fileURLWithPath: String(path)))
    print("\(result.root.fileCount) files · \(result.root.size) logical bytes · \(result.root.allocatedSize) allocated bytes")
    print(String(format: "Scanned in %.3f seconds; %d skipped", result.elapsed, result.skippedCount))
    for child in result.root.children.prefix(20) {
        print("\(child.size)\t\(child.name)\(child.isDirectory ? "/" : "")")
    }
    for issue in result.issues { fputs("\(issue)\n", stderr) }
} catch {
    fputs("\(error.localizedDescription)\n", stderr)
    exit(1)
}
