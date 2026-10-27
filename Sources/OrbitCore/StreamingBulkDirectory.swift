import Darwin
import Foundation

package enum StreamingBulkDirectory {
    // One worker can reuse the native buffer across listings; concurrent workers need separate buffers.
    package final class Buffer {
        fileprivate let capacity = 256 * 1024
        fileprivate let storage = UnsafeMutableRawPointer.allocate(byteCount: 256 * 1024, alignment: 8)
        deinit { storage.deallocate() }
    }

    enum Failure: Error { case needsFallback }
    static let common = ATTR_CMN_RETURNED_ATTRS | UInt32(ATTR_CMN_ERROR | ATTR_CMN_NAME |
                               ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID)
    static let file = UInt32(ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH)

    package static func stream(_ descriptor: Int32, url: URL, cancellation: ScanCancellation,
                     progress: (Int, Int64) -> Void, issue: (String) -> Void,
                     forceFallback: Bool = false,
                     testFallbackAfterFirstBuffer: (() -> Void)? = nil,
                     buffer reusableBuffer: Buffer? = nil,
                     batch: ([FileMetadata]) throws -> Void) throws {
        if forceFallback {
            try fallback(descriptor, url: url, cancellation: cancellation, progress: progress, issue: issue, batch: batch)
            return
        }
        var attributes = attrlist(bitmapcount: UInt16(ATTR_BIT_MAP_COUNT), reserved: 0,
                                  commonattr: common, volattr: 0, dirattr: UInt32(ATTR_DIR_MOUNTSTATUS),
                                  fileattr: file, forkattr: 0)
        let ownedBuffer = reusableBuffer ?? Buffer()
        defer { withExtendedLifetime(ownedBuffer) {} }
        let capacity = ownedBuffer.capacity
        let buffer = ownedBuffer.storage
        var entries: [FileMetadata] = []
        var bytes: Int64 = 0
        var itemCount = 0
        func accept(_ metadata: FileMetadata) throws {
            entries.append(metadata)
            itemCount += 1
            bytes += metadata.size
            if entries.count == 128 {
                try batch(entries)
                entries.removeAll(keepingCapacity: true)
            }
        }
        while true {
            if cancellation.isCancelled { throw CancellationError() }
            let count = getattrlistbulk(descriptor, &attributes, buffer, capacity, UInt64(FSOPT_PACK_INVAL_ATTRS))
            if count < 0 {
                let failure = errno
                if failure == ENOTSUP || failure == EINVAL || failure == ENOSYS {
                    throw Failure.needsFallback
                }
                throw ScanError.filesystem(url.path, failure)
            }
            if count == 0 {
                if !entries.isEmpty { try batch(entries) }
                progress(itemCount, bytes)
                return
            }
            var offset = 0
            for index in 0..<count {
                if index & 127 == 0, cancellation.isCancelled { throw CancellationError() }
                guard offset + 4 <= capacity else { throw ScanError.filesystem(url.path, EIO) }
                let record = UnsafeRawPointer(buffer.advanced(by: offset))
                let length = Int(record.loadUnaligned(as: UInt32.self))
                guard length >= 52, length <= capacity - offset else { throw ScanError.filesystem(url.path, EIO) }
                defer { offset += length }
                let returned = record.loadUnaligned(fromByteOffset: 4, as: attribute_set_t.self)
                let error = record.loadUnaligned(fromByteOffset: 24, as: UInt32.self)
                let reference = record.loadUnaligned(fromByteOffset: 28, as: attrreference_t.self)
                let nameStart = 28 + Int(reference.attr_dataoffset)
                let nameLength = Int(reference.attr_length)
                guard returned.commonattr & UInt32(ATTR_CMN_NAME) != 0,
                      nameStart >= 52, nameLength > 0, nameStart <= length,
                      nameLength <= length - nameStart,
                      record.load(fromByteOffset: nameStart + nameLength - 1, as: UInt8.self) == 0 else {
                    throw ScanError.filesystem(url.path, EIO)
                }
                let nameBytes = UnsafeBufferPointer(start: record.advanced(by: nameStart).assumingMemoryBound(to: UInt8.self),
                                                    count: nameLength - 1)
                guard let name = String(bytes: nameBytes, encoding: .utf8), !name.contains("/") else {
                    issue("\(url.path): file name could not be decoded")
                    continue
                }
                if name == "." || name == ".." { continue }
                if returned.commonattr & UInt32(ATTR_CMN_ERROR) != 0, error != 0 {
                    let childURL = url.appendingPathComponent(name, isDirectory: false)
                    issue(ScanError.filesystem(childURL.path, Int32(bitPattern: error)).localizedDescription)
                    continue
                }
                let required = UInt32(ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID)
                let kind = record.loadUnaligned(fromByteOffset: 40, as: UInt32.self)
                let isDirectory = kind == VDIR.rawValue
                let complete = returned.commonattr & required == required &&
                    (isDirectory ? returned.dirattr & UInt32(ATTR_DIR_MOUNTSTATUS) != 0 : returned.fileattr & file == file)
                if !complete {
                    if cancellation.isCancelled { throw CancellationError() }
                    let childURL = url.appendingPathComponent(name, isDirectory: false)
                    let metadata: FileMetadata
                    do { metadata = try readAt(descriptor, name: name, path: childURL.path) }
                    catch { issue(error.localizedDescription); continue }
                    try accept(metadata)
                    continue
                }
                // Darwin omits the entire file/dir group for other object types, even with PACK_INVAL_ATTRS.
                guard nameStart >= (isDirectory ? 56 : 72) else { throw ScanError.filesystem(url.path, EIO) }
                let mount = isDirectory ? record.loadUnaligned(fromByteOffset: 52, as: UInt32.self) : 0
                try accept(FileMetadata(
                    name: name, device: record.loadUnaligned(fromByteOffset: 36, as: Int32.self),
                    inode: record.loadUnaligned(fromByteOffset: 44, as: UInt64.self),
                    isDirectory: isDirectory, isSymbolicLink: kind == VLNK.rawValue,
                    isMountPoint: isDirectory && mount & UInt32(DIR_MNTSTATUS_MNTPOINT) != 0,
                    linkCount: isDirectory ? 1 : record.loadUnaligned(fromByteOffset: 52, as: UInt32.self),
                    size: isDirectory ? 0 : max(0, record.loadUnaligned(fromByteOffset: 64, as: Int64.self)),
                    allocatedSize: isDirectory ? 0 : max(0, record.loadUnaligned(fromByteOffset: 56, as: Int64.self))))
            }
            progress(itemCount, bytes)
            if let testFallbackAfterFirstBuffer { testFallbackAfterFirstBuffer(); throw Failure.needsFallback }
        }
    }
    private static func readAt(_ descriptor: Int32, name: String, path: String) throws -> FileMetadata {
        var info = stat()
        guard name.withCString({ fstatat(descriptor, $0, &info, AT_SYMLINK_NOFOLLOW) }) == 0 else {
            throw ScanError.filesystem(path, errno)
        }
        let kind = info.st_mode & mode_t(S_IFMT)
        let directory = kind == mode_t(S_IFDIR)
        var isMountPoint = false
        if directory {
            let child = name.withCString { openat(descriptor, $0, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC) }
            if child >= 0 {
                defer { close(child) }
                var childInfo = stat()
                guard fstat(child, &childInfo) == 0, childInfo.st_dev == info.st_dev, childInfo.st_ino == info.st_ino else {
                    throw ScanError.filesystem(path, EAGAIN)
                }
                var attributes = attrlist(bitmapcount: UInt16(ATTR_BIT_MAP_COUNT), reserved: 0,
                                          commonattr: 0, volattr: 0, dirattr: UInt32(ATTR_DIR_MOUNTSTATUS), fileattr: 0, forkattr: 0)
                var value: [UInt32] = [0, 0]
                let status = value.withUnsafeMutableBytes { fgetattrlist(child, &attributes, $0.baseAddress, $0.count, 0) }
                if status == 0 && value[0] >= 8 { isMountPoint = value[1] & UInt32(DIR_MNTSTATUS_MNTPOINT) != 0 }
                else {
                    var parentFS = statfs(), childFS = statfs()
                    guard fstatfs(descriptor, &parentFS) == 0, fstatfs(child, &childFS) == 0 else { throw ScanError.filesystem(path, errno) }
                    isMountPoint = parentFS.f_fsid.val.0 != childFS.f_fsid.val.0 || parentFS.f_fsid.val.1 != childFS.f_fsid.val.1
                }
            } else if errno != EACCES && errno != EPERM {
                throw ScanError.filesystem(path, errno)
            }
        }
        return FileMetadata(name: name, device: info.st_dev, inode: info.st_ino,
                            isDirectory: directory, isSymbolicLink: kind == mode_t(S_IFLNK),
                            isMountPoint: isMountPoint, linkCount: UInt32(info.st_nlink),
                            size: directory ? 0 : max(0, info.st_size),
                            allocatedSize: directory ? 0 : max(0, info.st_blocks * 512))
    }

    private static func fallback(_ descriptor: Int32, url: URL, cancellation: ScanCancellation,
                                 progress: (Int, Int64) -> Void, issue: (String) -> Void,
                                 batch: ([FileMetadata]) throws -> Void) throws {
        let fd = openat(descriptor, ".", O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard fd >= 0 else { throw ScanError.filesystem(url.path, errno) }
        guard let directory = fdopendir(fd) else { let code = errno; close(fd); throw ScanError.filesystem(url.path, code) }
        defer { closedir(directory) }
        var entries: [FileMetadata] = [], count = 0, bytes: Int64 = 0
        while true {
            if cancellation.isCancelled { throw CancellationError() }
            errno = 0
            guard let entry = readdir(directory) else {
                if errno != 0 { throw ScanError.filesystem(url.path, errno) }
                if !entries.isEmpty { try batch(entries) }
                progress(count, bytes)
                return
            }
            let name = withUnsafePointer(to: &entry.pointee.d_name) {
                $0.withMemoryRebound(to: UInt8.self, capacity: Int(entry.pointee.d_namlen)) {
                    String(bytes: UnsafeBufferPointer(start: $0, count: Int(entry.pointee.d_namlen)), encoding: .utf8)
                }
            }
            guard let name, !name.contains("/") else { issue("\(url.path): file name could not be decoded"); continue }
            if name == "." || name == ".." { continue }
            do {
                let item = try readAt(fd, name: name, path: url.appendingPathComponent(name).path)
                entries.append(item); count += 1; bytes += item.size
            } catch { issue(error.localizedDescription); continue }
            if entries.count == 128 { try batch(entries); entries.removeAll(keepingCapacity: true); progress(count, bytes) }
        }
    }

}
