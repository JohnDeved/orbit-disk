import Darwin
import Foundation

struct VolumeTraversalScope {
    private let rootPath: Data
    private let rootIdentity: DirectoryIdentity
    private let rootMount: String?
    private let boot: BootVolumeLayout?

    init(rootURL: URL, rootIdentity: DirectoryIdentity) {
        rootPath = Data(rootURL.standardizedFileURL.path.utf8)
        self.rootIdentity = rootIdentity
        rootMount = BootVolumeLayout.mount(at: rootURL.path)
        if rootPath == Data("/".utf8),
           BootVolumeLayout.directory(at: "/") == rootIdentity {
            boot = BootVolumeLayout.capture()
        } else {
            boot = nil
        }
    }

    func allows(path: Data, device: Int32, isMountPoint: Bool) -> Bool {
        guard contains(path, root: rootPath), !excludes(path: path) else { return false }
        if isMountPoint {
            return path == BootVolumeLayout.dataPath && boot?.dataIdentity.device == device
        }
        return device == rootIdentity.device || permitsData(path: path, device: device)
    }

    func excludes(path: Data) -> Bool {
        guard let boot, contains(path, root: BootVolumeLayout.dataPath) else { return false }
        return boot.physicalAliases.contains(where: { contains(path, root: $0) })
    }

    // Callers must still compare the opened descriptor with the queued identity.
    func allowsIdentity(path: Data, identity: DirectoryIdentity) -> Bool {
        guard contains(path, root: rootPath), !excludes(path: path) else { return false }
        if path == rootPath { return identity == rootIdentity }
        if let boot {
            if path == BootVolumeLayout.dataPath { return identity == boot.dataIdentity }
            if let expected = boot.logicalAliases[path] { return identity == expected }
        }
        return identity.device == rootIdentity.device || permitsData(path: path, device: identity.device)
    }

    func allowsMount(path: Data, mount: String) -> Bool {
        guard contains(path, root: rootPath), !excludes(path: path) else { return false }
        if mount == rootMount { return true }
        guard let boot, mount == BootVolumeLayout.dataVolume else { return false }
        return contains(path, root: BootVolumeLayout.dataPath)
            || boot.logicalAliases.keys.contains(where: { contains(path, root: $0) })
    }

    private func permitsData(path: Data, device: Int32) -> Bool {
        guard let boot, device == boot.dataIdentity.device else { return false }
        return contains(path, root: BootVolumeLayout.dataPath)
            || boot.logicalAliases.keys.contains(where: { contains(path, root: $0) })
    }

    private func contains(_ path: Data, root: Data) -> Bool {
        path == root || (path.starts(with: root) &&
            (root == Data("/".utf8) || (path.count > root.count && path[root.count] == 47)))
    }
}

private struct BootVolumeLayout {
    static let dataVolume = "/System/Volumes/Data"
    static let dataPath = Data(dataVolume.utf8)
    let dataIdentity: DirectoryIdentity
    let logicalAliases: [Data: DirectoryIdentity]
    let physicalAliases: [Data]

    struct Link {
        let logical: String
        let physical: String
    }

    // The sealed system's table identifies the only aliases we may normalize.
    static let links: [Link] = {
        guard let text = try? String(contentsOfFile: "/usr/share/firmlinks", encoding: .utf8) else { return [] }
        return text.split(whereSeparator: \.isNewline).compactMap { line in
            let fields = line.split(whereSeparator: \.isWhitespace)
            guard fields.count == 2, fields[0].first == "/", fields[1].first != "/" else { return nil }
            let logical = String(fields[0]), relative = String(fields[1])
            guard safeComponents(logical.dropFirst()), safeComponents(relative[...]) else { return nil }
            return Link(logical: logical, physical: dataVolume + "/" + relative)
        }
    }()

    static func capture() -> BootVolumeLayout? {
        guard mount(at: "/") == "/", mount(at: dataVolume) == dataVolume,
              let dataIdentity = directory(at: dataVolume) else { return nil }
        var logical: [Data: DirectoryIdentity] = [:], physical: [Data] = []
        for link in links {
            guard let identity = verifiedIdentity(link) else { continue }
            logical[Data(link.logical.utf8)] = identity
            physical.append(Data(link.physical.utf8))
        }
        // A mounted directory named Data alone is not evidence of the boot volume group.
        guard logical[Data("/Users".utf8)] != nil else { return nil }
        return BootVolumeLayout(dataIdentity: dataIdentity, logicalAliases: logical, physicalAliases: physical)
    }

    static func verifiedIdentity(_ link: Link) -> DirectoryIdentity? {
        guard let logical = directory(at: link.logical), let physical = directory(at: link.physical),
              logical == physical, mount(at: link.logical) == dataVolume,
              mount(at: link.physical) == dataVolume else { return nil }
        return logical
    }

    static func directory(at path: String) -> DirectoryIdentity? {
        var info = stat()
        guard lstat(path, &info) == 0, info.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR) else { return nil }
        return DirectoryIdentity(device: info.st_dev, inode: info.st_ino)
    }

    static func mount(at path: String) -> String? {
        var filesystem = statfs()
        guard statfs(path, &filesystem) == 0 else { return nil }
        return withUnsafePointer(to: &filesystem.f_mntonname) {
            String(cString: UnsafeRawPointer($0).assumingMemoryBound(to: CChar.self))
        }
    }

    private static func safeComponents(_ path: Substring) -> Bool {
        !path.isEmpty && !path.utf8.contains(0)
            && path.split(separator: "/", omittingEmptySubsequences: false).allSatisfy { !$0.isEmpty && $0 != "." && $0 != ".." }
    }
}

struct BootFirmlinkMetadata {
    private let links: [String: BootVolumeLayout.Link]

    private static let directories: [String: [String: BootVolumeLayout.Link]] = {
        var result: [String: [String: BootVolumeLayout.Link]] = [:]
        for link in BootVolumeLayout.links {
            let url = URL(fileURLWithPath: link.logical)
            result[url.deletingLastPathComponent().path, default: [:]][url.lastPathComponent] = link
        }
        return result
    }()

    static func forDirectory(_ url: URL) -> BootFirmlinkMetadata? {
        guard let links = directories[url.path] else { return nil }
        return BootFirmlinkMetadata(links: links)
    }

    func normalize(_ metadata: FileMetadata, descriptor: Int32) throws -> FileMetadata {
        guard metadata.isDirectory, !metadata.isSymbolicLink, let link = links[metadata.name] else { return metadata }
        var opened = stat()
        guard metadata.name.withCString({ fstatat(descriptor, $0, &opened, AT_SYMLINK_NOFOLLOW) }) == 0 else {
            throw ScanError.filesystem(link.logical, errno)
        }
        guard opened.st_mode & mode_t(S_IFMT) == mode_t(S_IFDIR),
              let expected = BootVolumeLayout.verifiedIdentity(link),
              expected == DirectoryIdentity(device: opened.st_dev, inode: opened.st_ino) else {
            throw ScanError.filesystem(link.logical, EAGAIN)
        }
        return FileMetadata(name: metadata.name, device: opened.st_dev, inode: opened.st_ino,
                            isDirectory: true, isSymbolicLink: false, isMountPoint: false,
                            linkCount: metadata.linkCount, size: metadata.size, allocatedSize: metadata.allocatedSize)
    }
}
