import Foundation
import OrbitCore

final class TrashPolicyTests: CheckCase {
    private var fixture: URL!
    private var scanRoot: URL!
    private var home: URL!

    override func setUpWithError() throws {
        fixture = FileManager.default.temporaryDirectory.appendingPathComponent("OrbitTrashPolicy-\(UUID().uuidString)")
        home = fixture.appendingPathComponent("home")
        scanRoot = home.appendingPathComponent("Downloads")
        try FileManager.default.createDirectory(at: scanRoot, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        if let fixture { try FileManager.default.removeItem(at: fixture) }
    }

    func testOrdinaryDescendantIsAllowedButRootAndSiblingAreProtected() throws {
        let file = try makeFile("report.txt")
        checkNil(reason(file))
        checkNotNil(reason(scanRoot))
        let sibling = home.appendingPathComponent("Downloads-old")
        try FileManager.default.createDirectory(at: sibling, withIntermediateDirectories: true)
        checkNotNil(reason(sibling))
        checkNotNil(TrashPolicy.rejectionReason(for: home, scanRoot: fixture, home: home))
    }

    func testSymlinkAndAncestorEscapeAreRejected() throws {
        let file = try makeFile("report.txt")
        let link = scanRoot.appendingPathComponent("report-link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: file)
        checkNotNil(reason(link))
        let outside = fixture.appendingPathComponent("outside")
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        try Data("outside".utf8).write(to: outside.appendingPathComponent("item"))
        let escaped = scanRoot.appendingPathComponent("escape")
        try FileManager.default.createSymbolicLink(at: escaped, withDestinationURL: outside)
        checkNotNil(reason(escaped.appendingPathComponent("item")))
    }

    func testHiddenDirectoriesTrashAndLibraryAreProtected() throws {
        for folder in [".ssh", ".Trash", ".Trashes"] {
            let directory = scanRoot.appendingPathComponent(folder)
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let file = directory.appendingPathComponent("item")
            try Data().write(to: file)
            checkNotNil(reason(directory))
            checkNotNil(reason(file))
        }
        let library = home.appendingPathComponent("Library")
        try FileManager.default.createDirectory(at: library, withIntermediateDirectories: true)
        checkNotNil(TrashPolicy.rejectionReason(for: library, scanRoot: home, home: home))
        checkNil(reason(try makeFile(".DS_Store")))
    }

    func testSystemPathsVolumesAndMissingItemsAreRejected() {
        for path in ["/", "/System", "/Library", "/Applications", "/usr", "/bin", "/sbin", "/etc", "/private", "/private/var", "/Volumes/External"] {
            checkNotNil(TrashPolicy.rejectionReason(for: URL(fileURLWithPath: path), scanRoot: URL(fileURLWithPath: "/"), home: home), path)
        }
        checkNotNil(reason(scanRoot.appendingPathComponent("missing")))
        checkNotNil(TrashPolicy.rejectionReason(for: URL(string: "https://example.com/item")!, scanRoot: scanRoot, home: home))
    }

    private func reason(_ url: URL) -> String? {
        TrashPolicy.rejectionReason(for: url, scanRoot: scanRoot, home: home)
    }

    private func makeFile(_ name: String) throws -> URL {
        let file = scanRoot.appendingPathComponent(name)
        try Data("test".utf8).write(to: file)
        return file
    }
}
