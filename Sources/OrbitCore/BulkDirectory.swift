import Darwin
import Foundation

package enum BulkDirectory {
    static let common = ATTR_CMN_RETURNED_ATTRS | UInt32(ATTR_CMN_ERROR | ATTR_CMN_NAME |
                               ATTR_CMN_DEVID | ATTR_CMN_OBJTYPE | ATTR_CMN_FILEID)
    static let file = UInt32(ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE | ATTR_FILE_DATALENGTH)

    package static func read(_ descriptor: Int32, url: URL, cancellation: ScanCancellation,
                     progress: (Int, Int64) -> Void, issue: (String) -> Void) throws -> [FileMetadata] {
        var attributes = attrlist(bitmapcount: UInt16(ATTR_BIT_MAP_COUNT), reserved: 0,
                                  commonattr: common, volattr: 0, dirattr: UInt32(ATTR_DIR_MOUNTSTATUS),
                                  fileattr: file, forkattr: 0)
        let capacity = 256 * 1024
        let buffer = UnsafeMutableRawPointer.allocate(byteCount: capacity, alignment: 8)
        defer { buffer.deallocate() }
        var entries: [FileMetadata] = []
        var bytes: Int64 = 0
        while true {
            if cancellation.isCancelled { throw CancellationError() }
            let count = getattrlistbulk(descriptor, &attributes, buffer, capacity, UInt64(FSOPT_PACK_INVAL_ATTRS))
            guard count >= 0 else { throw ScanError.filesystem(url.path, errno) }
            if count == 0 { return entries }
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
                    do {
                        let metadata = try FileMetadata.read(childURL)
                        entries.append(metadata)
                        bytes += metadata.size
                    }
                    catch { issue(error.localizedDescription) }
                    continue
                }
                // Darwin omits the entire file/dir group for other object types, even with PACK_INVAL_ATTRS.
                guard nameStart >= (isDirectory ? 56 : 72) else { throw ScanError.filesystem(url.path, EIO) }
                let mount = isDirectory ? record.loadUnaligned(fromByteOffset: 52, as: UInt32.self) : 0
                entries.append(FileMetadata(
                    name: name, device: record.loadUnaligned(fromByteOffset: 36, as: Int32.self),
                    inode: record.loadUnaligned(fromByteOffset: 44, as: UInt64.self),
                    isDirectory: isDirectory, isSymbolicLink: kind == VLNK.rawValue,
                    isMountPoint: isDirectory && mount & UInt32(DIR_MNTSTATUS_MNTPOINT) != 0,
                    linkCount: isDirectory ? 1 : record.loadUnaligned(fromByteOffset: 52, as: UInt32.self),
                    size: isDirectory ? 0 : max(0, record.loadUnaligned(fromByteOffset: 64, as: Int64.self)),
                    allocatedSize: isDirectory ? 0 : max(0, record.loadUnaligned(fromByteOffset: 56, as: Int64.self))))
                bytes += entries[entries.count - 1].size
            }
            progress(entries.count, bytes)
        }
    }
}
