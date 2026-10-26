import Foundation
import CoreServices
import IOKit
import Darwin

struct DiskChangeCheckpoint: Codable, Equatable, Sendable {
    let rootPath: String
    let identity: String
    let eventID: UInt64
}

struct DiskChangeBatch: Sendable {
    /// Reconcile each directory recursively; paths may have been removed or renamed.
    let directoryPaths: Set<String>
    let fullRefreshRequired: Bool
    let historyFinished: Bool
    /// Persist only after applying every change through this batch to the saved tree.
    let checkpoint: DiskChangeCheckpoint
}

enum DiskChangeMonitorError: LocalizedError {
    case invalidRoot(String)
    case unavailable(String)
    case historyUnavailable

    var errorDescription: String? {
        switch self {
        case .invalidRoot(let path): return "Cannot monitor this folder: \(path)"
        case .unavailable(let path): return "File change monitoring is unavailable for \(path)."
        case .historyUnavailable: return "This volume does not provide reusable change history. Use Rescan to update the map."
        }
    }
}

@MainActor
final class DiskChangeMonitor {
    private let root: URL
    private let exclusions: [URL]
    private var plan: DiskEventPlan?
    private var excludedPaths: [String] = []
    private var stream: DiskEventStream?
    private var context: DiskEventContext?
    private var handler: ((DiskChangeBatch) -> Void)?
    private var latestEventID: UInt64 = 0
    private var checkpointIdentity = ""
    private var generation = UUID()

    nonisolated static func capture(root: URL) throws -> DiskChangeCheckpoint {
        try DiskEventPlan.inspect(root: root).checkpoint
    }

    init(root: URL, excluding: [URL] = []) throws {
        guard root.isFileURL, root.path.hasPrefix("/"), !root.path.contains("\0") else {
            throw DiskChangeMonitorError.invalidRoot(root.absoluteString)
        }
        self.root = root.standardizedFileURL
        exclusions = excluding
    }

    /// False means the saved history cannot be replayed; a full-refresh batch follows.
    @discardableResult
    func start(sinceEventID: UInt64, eventIdentity: String,
               onChange: @escaping (DiskChangeBatch) -> Void) async throws -> Bool {
        stop()
        let token = generation
        handler = onChange
        let callbackContext = DiskEventContext(monitor: self)
        context = callbackContext
        let root = root, exclusions = exclusions
        // WatchRoot may wait for an ancestor's macOS privacy prompt during creation.
        let worker = Task.detached(priority: .utility) {
            try DiskEventSetup.prepare(root: root, excluding: exclusions, sinceEventID: sinceEventID,
                                       eventIdentity: eventIdentity, context: callbackContext)
        }
        do {
            let prepared = try await withTaskCancellationHandler {
                try await worker.value
            } onCancel: {
                worker.cancel()
            }
            guard generation == token, !Task.isCancelled else {
                Task.detached { prepared.stream.close() }
                throw CancellationError()
            }
            plan = prepared.plan
            excludedPaths = prepared.excludedPaths
            latestEventID = prepared.since
            checkpointIdentity = prepared.plan.checkpoint.identity
            stream = prepared.stream
            callbackContext.activate()
            guard generation == token else { throw CancellationError() }
            if !prepared.canReplay {
                handler?(DiskChangeBatch(directoryPaths: [prepared.plan.rootPath],
                    fullRefreshRequired: true, historyFinished: false,
                    checkpoint: prepared.plan.checkpoint))
            }
            return prepared.canReplay
        } catch {
            if generation == token { stop() }
            throw error
        }
    }

    func stop() {
        generation = UUID()
        context?.cancel()
        if let stream { Task.detached { stream.close() } }
        stream = nil
        plan = nil
        context = nil
        handler = nil
    }

    deinit {
        if let stream { Task.detached { stream.close() } }
    }

    fileprivate func receive(paths: [String], flags: [FSEventStreamEventFlags], ids: [UInt64]) {
        guard stream != nil, let plan else { return }
        var directories = Set<String>()
        var fullRefresh = paths.count != flags.count || flags.count != ids.count
        var historyFinished = false
        var identityInvalidated = false
        for index in flags.indices {
            let flag = flags[index]
            let has: (Int) -> Bool = { flag & FSEventStreamEventFlags($0) != 0 }
            if index < ids.count, ids[index] != UInt64(kFSEventStreamEventIdSinceNow) {
                latestEventID = max(latestEventID, ids[index])
            }
            if has(kFSEventStreamEventFlagEventIdsWrapped) || has(kFSEventStreamEventFlagRootChanged) {
                fullRefresh = true
                identityInvalidated = true
            }
            if has(kFSEventStreamEventFlagUserDropped) || has(kFSEventStreamEventFlagKernelDropped) {
                fullRefresh = true
            }
            if has(kFSEventStreamEventFlagHistoryDone) { historyFinished = true; continue }
            guard index < paths.count, let path = plan.normalizedEventPath(paths[index]),
                  !excludedPaths.contains(where: { DiskEventPlan.contains(path, in: $0) }) else { continue }
            if has(kFSEventStreamEventFlagMount) || has(kFSEventStreamEventFlagUnmount) {
                fullRefresh = true
                identityInvalidated = true
            }
            if plan.untrackedMounts.contains(where: { DiskEventPlan.contains(path, in: $0) }) { continue }
            let directory: String
            if has(kFSEventStreamEventFlagMustScanSubDirs) {
                directory = path
            } else if has(kFSEventStreamEventFlagItemIsDir)
                        && !has(kFSEventStreamEventFlagItemRemoved)
                        && !has(kFSEventStreamEventFlagItemRenamed) {
                directory = path
            } else {
                directory = URL(fileURLWithPath: path).deletingLastPathComponent().path
            }
            directories.insert(DiskEventPlan.contains(directory, in: plan.rootPath) ? directory : plan.rootPath)
        }
        if identityInvalidated {
            // The required full scan captures a new identity away from the main thread.
            checkpointIdentity = ""
            latestEventID = 0
        }
        if fullRefresh { directories = [plan.rootPath] }
        let checkpoint = DiskChangeCheckpoint(rootPath: plan.rootPath,
                                              identity: checkpointIdentity, eventID: latestEventID)
        // Reporting cache-only batches would let saving their watermark trigger another save.
        guard !directories.isEmpty || fullRefresh || historyFinished else { return }
        handler?(DiskChangeBatch(directoryPaths: directories, fullRefreshRequired: fullRefresh,
                                 historyFinished: historyFinished, checkpoint: checkpoint))
    }
}

@MainActor
private final class DiskEventContext {
    private weak var monitor: DiskChangeMonitor?
    private var active = false
    private var pending: [([String], [FSEventStreamEventFlags], [UInt64])] = []
    init(monitor: DiskChangeMonitor) { self.monitor = monitor }

    func receive(paths: [String], flags: [FSEventStreamEventFlags], ids: [UInt64]) {
        guard let monitor else { return }
        if active { monitor.receive(paths: paths, flags: flags, ids: ids) }
        else { pending.append((paths, flags, ids)) }
    }

    func activate() {
        active = true
        let batches = pending
        pending.removeAll()
        for (paths, flags, ids) in batches { monitor?.receive(paths: paths, flags: flags, ids: ids) }
    }

    func cancel() {
        monitor = nil
        pending.removeAll()
    }
}

private final class DiskEventStream: @unchecked Sendable {
    private let lock = NSLock()
    private var native: FSEventStreamRef?

    init(_ native: FSEventStreamRef) { self.native = native }

    func close() {
        lock.lock()
        let stream = native
        native = nil
        lock.unlock()
        if let stream {
            FSEventStreamStop(stream)
            FSEventStreamInvalidate(stream)
            FSEventStreamRelease(stream)
        }
    }

    deinit { close() }
}

private struct DiskEventSetup: Sendable {
    let plan: DiskEventPlan
    let excludedPaths: [String]
    let since: UInt64
    let canReplay: Bool
    let stream: DiskEventStream

    static func prepare(root: URL, excluding: [URL], sinceEventID: UInt64,
                        eventIdentity: String, context: DiskEventContext) throws -> DiskEventSetup {
        try Task.checkCancellation()
        let plan = try DiskEventPlan.inspect(root: root)
        // A live-only stream cannot close the baseline-scan gap; retrying full scans would loop.
        guard plan.supportsReplay else { throw DiskChangeMonitorError.historyUnavailable }
        // The time-based device query can lag already-delivered events; it is not an upper bound.
        let canReplay = eventIdentity == plan.checkpoint.identity
            && sinceEventID <= FSEventsGetCurrentEventId()
            && sinceEventID != UInt64(kFSEventStreamEventIdSinceNow)
        let since = canReplay ? sinceEventID : plan.checkpoint.eventID
        let excludedPaths = Array(Set(excluding.flatMap {
            [$0.standardizedFileURL.path, $0.standardizedFileURL.resolvingSymlinksInPath().path]
        }))
        try Task.checkCancellation()
        var nativeContext = FSEventStreamContext(
            version: 0, info: Unmanaged.passUnretained(context).toOpaque(),
            retain: { pointer in
                guard let pointer else { return nil }
                _ = Unmanaged<DiskEventContext>.fromOpaque(pointer).retain()
                return pointer
            },
            release: { pointer in
                if let pointer { Unmanaged<DiskEventContext>.fromOpaque(pointer).release() }
            }, copyDescription: nil
        )
        let flags = FSEventStreamCreateFlags(kFSEventStreamCreateFlagUseCFTypes
            | kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagWatchRoot
            | kFSEventStreamCreateFlagNoDefer)
        let created = withExtendedLifetime(context) {
            if let relativePath = plan.relativePath {
                return FSEventStreamCreateRelativeToDevice(nil, diskEventCallback, &nativeContext,
                    plan.device, [relativePath] as CFArray, since, 0.6, flags)
            } else {
                return FSEventStreamCreate(nil, diskEventCallback, &nativeContext,
                    [plan.canonicalRoot] as CFArray, since, 0.6, flags)
            }
        }
        guard let created else { throw DiskChangeMonitorError.unavailable(plan.rootPath) }
        let stream = DiskEventStream(created)
        try Task.checkCancellation()
        FSEventStreamSetDispatchQueue(created, .main)
        guard FSEventStreamStart(created) else { throw DiskChangeMonitorError.unavailable(plan.rootPath) }
        try Task.checkCancellation()
        return DiskEventSetup(plan: plan, excludedPaths: excludedPaths, since: since,
                              canReplay: canReplay, stream: stream)
    }
}

private let diskEventCallback: FSEventStreamCallback = { _, info, count, rawPaths, flags, ids in
    guard let info else { return }
    let context = Unmanaged<DiskEventContext>.fromOpaque(info).takeUnretainedValue()
    let paths = Unmanaged<CFArray>.fromOpaque(rawPaths).takeUnretainedValue() as? [String] ?? []
    let copiedFlags = Array(UnsafeBufferPointer(start: flags, count: count))
    let copiedIDs = Array(UnsafeBufferPointer(start: ids, count: count))
    MainActor.assumeIsolated {
        context.receive(paths: paths, flags: copiedFlags, ids: copiedIDs)
    }
}

private struct DiskEventPlan: Sendable {
    struct Volume: Encodable {
        let mountPath: String
        let volumeUUID: String?
        let eventsUUID: String?
        let readOnly: Bool
    }
    struct Identity: Encodable {
        let version = 1
        let rootPath: String
        let canonicalRoot: String
        let rootFileID: UInt64
        let mode: String
        let hostID: String?
        let volumes: [Volume]
    }

    let rootPath: String
    let canonicalRoot: String
    let physicalRoot: String
    let mountPath: String
    let relativePath: String?
    let untrackedMounts: [String]
    let device: dev_t
    let supportsReplay: Bool
    let checkpoint: DiskChangeCheckpoint

    static func inspect(root: URL) throws -> DiskEventPlan {
        guard root.isFileURL else { throw DiskChangeMonitorError.invalidRoot(root.absoluteString) }
        let rootPath = root.standardizedFileURL.path
        let canonicalRoot = root.standardizedFileURL.resolvingSymlinksInPath().path
        guard rootPath.hasPrefix("/"), !rootPath.contains("\0"), !canonicalRoot.contains("\0") else {
            throw DiskChangeMonitorError.invalidRoot(rootPath)
        }
        var metadata = stat()
        var filesystem = statfs()
        guard stat(canonicalRoot, &metadata) == 0, metadata.st_mode & S_IFMT == S_IFDIR,
              statfs(canonicalRoot, &filesystem) == 0 else {
            throw DiskChangeMonitorError.invalidRoot(rootPath)
        }
        let mount = mountName(filesystem)
        var physical = canonicalRoot
        var relative: String?
        if rootPath != "/" {
            if contains(canonicalRoot, in: mount) {
                relative = String(canonicalRoot.dropFirst(mount == "/" ? 1 : mount.count)).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            } else {
                let candidate = URL(fileURLWithPath: mount).appendingPathComponent(canonicalRoot).path
                if sameFile(candidate, canonicalRoot) {
                    physical = candidate
                    relative = canonicalRoot.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
                }
            }
        }
        let hostID = relative == nil ? platformUUID() : nil
        let volumes: [Volume]
        if relative != nil {
            volumes = [volume(filesystem)]
        } else {
            var mounted: UnsafeMutablePointer<statfs>?
            let count = getmntinfo(&mounted, MNT_NOWAIT)
            guard let mounted, count > 0 else { throw DiskChangeMonitorError.unavailable(rootPath) }
            volumes = (0..<Int(count)).map { mounted[$0] }
                .filter { $0.f_flags & UInt32(MNT_LOCAL) != 0 }
                .map(volume).filter { $0.volumeUUID != nil || $0.eventsUUID != nil }
                .sorted { $0.mountPath < $1.mountPath }
        }
        // The scanner skips mounted subtrees; boot helper volumes have no journals to replay.
        let supportsReplay = !volumes.isEmpty && volumes.allSatisfy { $0.volumeUUID != nil }
            && (relative != nil ? volumes[0].eventsUUID != nil
                : hostID != nil && volumes.contains { $0.eventsUUID != nil })
        let untrackedMounts = relative == nil && rootPath == "/"
            ? volumes.map(\.mountPath).filter { $0 != "/" } : []
        let fingerprint = Identity(rootPath: rootPath, canonicalRoot: canonicalRoot,
                                   rootFileID: metadata.st_ino, mode: relative == nil ? "host" : "device",
                                   hostID: hostID, volumes: volumes)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let identity = String(decoding: try encoder.encode(fingerprint), as: UTF8.self)
        // This FSEvents API uses the Unix epoch despite naming its argument CFAbsoluteTime.
        let eventID = relative == nil ? FSEventsGetCurrentEventId()
            : FSEventsGetLastEventIdForDeviceBeforeTime(metadata.st_dev, Date().timeIntervalSince1970)
        return DiskEventPlan(rootPath: rootPath, canonicalRoot: canonicalRoot, physicalRoot: physical,
                             mountPath: mount, relativePath: relative, untrackedMounts: untrackedMounts,
                             device: metadata.st_dev,
                             supportsReplay: supportsReplay,
                             checkpoint: DiskChangeCheckpoint(rootPath: rootPath, identity: identity, eventID: eventID))
    }

    func normalizedEventPath(_ raw: String) -> String? {
        guard !raw.contains("\0"), !raw.split(separator: "/").contains("..") else { return nil }
        var path: String
        if relativePath != nil {
            path = URL(fileURLWithPath: mountPath).appendingPathComponent(raw).standardizedFileURL.path
            if Self.contains(path, in: physicalRoot) {
                path = rootPath + String(path.dropFirst(physicalRoot.count))
            } else if Self.contains(physicalRoot, in: path) {
                return rootPath
            } else { return nil }
        } else {
            guard raw.hasPrefix("/") else { return nil }
            path = URL(fileURLWithPath: raw).standardizedFileURL.path
            if canonicalRoot == "/", path.hasPrefix("/System/Volumes/Data/") {
                let alias = String(path.dropFirst("/System/Volumes/Data".count))
                if Self.matchesThroughExistingAncestor(path, alias) { path = alias }
            }
            if Self.contains(path, in: canonicalRoot) {
                path = rootPath == "/" ? path : rootPath + String(path.dropFirst(canonicalRoot.count))
            } else if Self.contains(canonicalRoot, in: path) {
                return rootPath
            } else { return nil }
        }
        return Self.contains(path, in: rootPath) ? path : nil
    }

    static func contains(_ path: String, in root: String) -> Bool {
        path == root || (root == "/" ? path.hasPrefix("/") : path.hasPrefix(root + "/"))
    }

    private static func mountName(_ filesystem: statfs) -> String {
        var name = filesystem.f_mntonname
        return withUnsafeBytes(of: &name) { String(cString: $0.baseAddress!.assumingMemoryBound(to: CChar.self)) }
    }

    private static func volume(_ filesystem: statfs) -> Volume {
        let mount = mountName(filesystem)
        var metadata = stat()
        let device = stat(mount, &metadata) == 0 ? metadata.st_dev : filesystem.f_fsid.val.0
        let uuid = try? URL(fileURLWithPath: mount).resourceValues(forKeys: [.volumeUUIDStringKey]).volumeUUIDString
        let eventsUUID = FSEventsCopyUUIDForDevice(device).map { CFUUIDCreateString(nil, $0) as String }
        return Volume(mountPath: mount, volumeUUID: uuid, eventsUUID: eventsUUID,
                      readOnly: filesystem.f_flags & UInt32(MNT_RDONLY) != 0)
    }

    private static func platformUUID() -> String? {
        let service = IOServiceGetMatchingService(kIOMainPortDefault, IOServiceMatching("IOPlatformExpertDevice"))
        guard service != 0 else { return nil }
        defer { IOObjectRelease(service) }
        return IORegistryEntryCreateCFProperty(service, "IOPlatformUUID" as CFString, nil, 0)?.takeRetainedValue() as? String
    }

    private static func sameFile(_ first: String, _ second: String) -> Bool {
        var a = stat()
        var b = stat()
        return stat(first, &a) == 0 && stat(second, &b) == 0 && a.st_dev == b.st_dev && a.st_ino == b.st_ino
    }

    private static func matchesThroughExistingAncestor(_ physical: String, _ alias: String) -> Bool {
        var first = physical
        var second = alias
        while second != "/" {
            if sameFile(first, second) { return true }
            first = URL(fileURLWithPath: first).deletingLastPathComponent().path
            second = URL(fileURLWithPath: second).deletingLastPathComponent().path
        }
        return false
    }
}
