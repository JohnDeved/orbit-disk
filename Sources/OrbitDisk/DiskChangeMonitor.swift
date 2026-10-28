import Foundation
import CoreServices
import IOKit
import Darwin
#if canImport(OrbitCore)
import OrbitCore
#endif

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
    private var rawExcludedPaths: [String] = []
    private var stream: DiskEventStream?
    private var context: DiskEventContext?
    private var handler: ((DiskChangeBatch) -> Void)?
    private var latestEventID: UInt64 = 0
    private var checkpointIdentity = ""
    private var generation = UUID()
    private let normalizationQueue = DispatchQueue(label: "OrbitDisk.EventNormalization", qos: .utility)
    private let normalizationGeneration = EventGeneration()
    private var queuedNormalizations = 0
    private var overflow: EventOverflow?

    nonisolated static func capture(root: URL) throws -> DiskChangeCheckpoint {
        try DiskEventPlan.inspect(root: root).checkpoint
    }

    #if canImport(OrbitCore)
    nonisolated static func reuseCandidate(_ snapshot: IndexSnapshot, for checkpoint: StageCheckpoint) throws -> StageReuseCandidate? {
        struct EventStream: Decodable, Equatable {
            let mode: String
            let volumes: [DiskEventPlan.Volume]
        }
        guard snapshot.skippedCount == 0 else { return nil }
        let source = try DiskEventPlan.inspect(root: snapshot.rootURL)
        let parent = try DiskEventPlan.inspect(root: URL(fileURLWithPath: checkpoint.rootPath, isDirectory: true))
        guard source.supportsReplay, parent.supportsReplay,
              source.checkpoint.identity == snapshot.eventIdentity,
              parent.checkpoint.identity == checkpoint.identity,
              snapshot.eventID <= source.checkpoint.eventID else { return nil }
        let sourceStream = try JSONDecoder().decode(EventStream.self, from: Data(source.checkpoint.identity.utf8))
        let parentStream = try JSONDecoder().decode(EventStream.self, from: Data(parent.checkpoint.identity.utf8))
        if source.relativePath != nil, parent.relativePath != nil,
           source.device == parent.device, source.mountPath == parent.mountPath, sourceStream == parentStream {
            return StageReuseCandidate(snapshot: snapshot,
                checkpoint: StageCheckpoint(rootPath: checkpoint.rootPath, identity: checkpoint.identity,
                                            eventID: min(checkpoint.eventID, snapshot.eventID)))
        }
        guard parent.rootPath == "/", parent.relativePath == nil, source.relativePath != nil,
              checkpoint.eventID <= parent.checkpoint.eventID,
              source.mountPath == "/System/Volumes/Data", sourceStream.volumes.count == 1,
              parentStream.volumes.contains(sourceStream.volumes[0]),
              try MappingCoverage.canReach(root: URL(fileURLWithPath: "/"), target: snapshot.rootURL,
                  expectedRootIdentity: "\(parent.device):\(parent.rootFileID)"),
              validateReuseHistory(source, since: snapshot.eventID),
              try DiskEventPlan.inspect(root: snapshot.rootURL).checkpoint.identity == snapshot.eventIdentity,
              try DiskEventPlan.inspect(root: URL(fileURLWithPath: "/")).checkpoint.identity == checkpoint.identity else { return nil }
        // Device and host cursors are never compared; the child's history has already been checked.
        return StageReuseCandidate(snapshot: snapshot, checkpoint: checkpoint, historyValidation: .verifiedThroughParentCheckpoint)
    }

    nonisolated private static func validateReuseHistory(_ plan: DiskEventPlan, since: UInt64) -> Bool {
        #if ORBIT_TESTING
        if let testReuseHistoryValidator { return testReuseHistoryValidator(URL(fileURLWithPath: plan.rootPath), since) }
        #endif
        return ReuseHistoryProbe.validate(plan: plan, since: since, timeout: 0.2)
    }

    #if ORBIT_TESTING
    nonisolated(unsafe) static var testReuseHistoryValidator: ((URL, UInt64) -> Bool)?
    nonisolated static func testReuseHistory(root: URL, since: UInt64, timeout: TimeInterval = 0.2) throws -> Bool {
        ReuseHistoryProbe.validate(plan: try DiskEventPlan.inspect(root: root), since: since, timeout: timeout)
    }
    nonisolated static func testReuseHistoryDecision(flags: [FSEventStreamEventFlags], flushed: Bool, expired: Bool = false) -> Bool {
        ReuseHistoryProbe.testDecision(flags: flags, flushed: flushed, expired: expired)
    }
    #endif
    #endif

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
            rawExcludedPaths = prepared.rawExcludedPaths
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
        normalizationGeneration.replace(with: generation)
        queuedNormalizations = 0
        overflow = nil
        context?.cancel()
        if let stream { Task.detached { stream.close() } }
        stream = nil
        plan = nil
        context = nil
        rawExcludedPaths = []
        handler = nil
    }

    deinit {
        if let stream { Task.detached { stream.close() } }
    }

    fileprivate func receive(paths: [String], flags: [FSEventStreamEventFlags], ids: [UInt64]) {
        guard stream != nil, let plan else { return }
        let token = generation
        if overflow != nil {
            overflow?.merge(excluded: rawExcludedPaths, paths: paths, flags: flags, ids: ids)
            return
        }
        if queuedNormalizations >= 8 {
            var merged = EventOverflow()
            merged.merge(excluded: rawExcludedPaths, paths: paths, flags: flags, ids: ids)
            overflow = merged
            let generationState = normalizationGeneration
            normalizationQueue.async { [weak self] in
                guard generationState.isCurrent(token) else { return }
                let rootValid = plan.rootIdentityIsValid()
                DispatchQueue.main.async { [weak self] in
                    self?.publishOverflow(token: token, rootValid: rootValid)
                }
            }
            return
        }
        queuedNormalizations += 1
        let excluded = excludedPaths
        let generationState = normalizationGeneration
        normalizationQueue.async { [weak self] in
            guard let result = NormalizedEventBatch.prepare(plan: plan, excluded: excluded, paths: paths,
                                                            flags: flags, ids: ids,
                                                            isCurrent: { generationState.isCurrent(token) }) else { return }
            DispatchQueue.main.async { [weak self] in
                guard let self, self.generation == token else { return }
                self.queuedNormalizations -= 1
                self.publish(result, token: token)
            }
        }
    }

    private func publishOverflow(token: UUID, rootValid: Bool) {
        guard generation == token, let overflow else { return }
        self.overflow = nil
        var result = NormalizedEventBatch()
        result.fullRefresh = overflow.requiresRefresh || !rootValid
        result.historyFinished = overflow.historyFinished
        result.identityInvalidated = overflow.identityInvalidated || !rootValid
        result.latestEventID = overflow.latestEventID
        publish(result, token: token)
    }

    private func publish(_ result: NormalizedEventBatch, token: UUID) {
        guard generation == token, stream != nil, let plan else { return }
        latestEventID = max(latestEventID, result.latestEventID)
        if result.identityInvalidated {
            checkpointIdentity = ""
            latestEventID = 0
        }
        let directories = result.fullRefresh ? Set([plan.rootPath]) : result.directories
        guard !directories.isEmpty || result.fullRefresh || result.historyFinished else { return }
        let checkpoint = DiskChangeCheckpoint(rootPath: plan.rootPath,
                                              identity: checkpointIdentity, eventID: latestEventID)
        handler?(DiskChangeBatch(directoryPaths: directories, fullRefreshRequired: result.fullRefresh,
                                 historyFinished: result.historyFinished, checkpoint: checkpoint))
    }

}

private final class ReuseHistoryProbe: @unchecked Sendable {
    private static let slots = DispatchSemaphore(value: 2)
    private let lock = NSLock()
    private let completion = DispatchSemaphore(value: 0)
    private let historyCompletion = DispatchSemaphore(value: 0)
    private var active = true
    private var historyFinished = false
    private var changed = false
    private var accepted = false

    private var pending: Bool { lock.lock(); defer { lock.unlock() }; return active }

    private func receive(_ flags: UnsafeBufferPointer<FSEventStreamEventFlags>) {
        lock.lock(); defer { lock.unlock() }
        guard active else { return }
        for flag in flags {
            let history = FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone)
            if flag == history {
                if !historyFinished { historyCompletion.signal() }
                historyFinished = true
            }
            else { changed = true }
        }
    }

    private func finish(_ valid: Bool) {
        lock.lock()
        guard active else { lock.unlock(); return }
        accepted = valid && historyFinished && !changed
        active = false
        lock.unlock()
        completion.signal()
    }

    private func wait(timeout: TimeInterval) -> Bool {
        let signalled = completion.wait(timeout: .now() + timeout) == .success
        lock.lock(); defer { lock.unlock() }
        active = false
        return signalled && accepted
    }

    #if ORBIT_TESTING
    static func testDecision(flags: [FSEventStreamEventFlags], flushed: Bool, expired: Bool) -> Bool {
        let probe = ReuseHistoryProbe()
        if expired { _ = probe.wait(timeout: 0) }
        flags.withUnsafeBufferPointer { probe.receive($0) }
        probe.finish(flushed)
        return probe.wait(timeout: 0)
    }
    #endif

    static func validate(plan: DiskEventPlan, since: UInt64, timeout: TimeInterval) -> Bool {
        guard timeout > 0, plan.relativePath != nil, plan.supportsReplay,
              since != UInt64(kFSEventStreamEventIdSinceNow), since <= plan.checkpoint.eventID,
              slots.wait(timeout: .now()) == .success else { return false }
        let probe = ReuseHistoryProbe()
        let deadline = DispatchTime.now() + timeout
        // Stream creation and flushing may block; discovery gives up after its small replay budget.
        DispatchQueue.global(qos: .utility).async {
            defer { probe.finish(false); slots.signal() }
            guard probe.pending else { return }
            var context = FSEventStreamContext(version: 0, info: Unmanaged.passUnretained(probe).toOpaque(),
                retain: { pointer in
                    guard let pointer else { return nil }
                    _ = Unmanaged<ReuseHistoryProbe>.fromOpaque(pointer).retain()
                    return pointer
                }, release: { pointer in
                    if let pointer { Unmanaged<ReuseHistoryProbe>.fromOpaque(pointer).release() }
                }, copyDescription: nil)
            let callback: FSEventStreamCallback = { _, info, count, _, flags, _ in
                guard let info else { return }
                Unmanaged<ReuseHistoryProbe>.fromOpaque(info).takeUnretainedValue()
                    .receive(UnsafeBufferPointer(start: flags, count: count))
            }
            let options = FSEventStreamCreateFlags(kFSEventStreamCreateFlagUseCFTypes
                | kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagWatchRoot | kFSEventStreamCreateFlagNoDefer)
            guard let stream = FSEventStreamCreateRelativeToDevice(nil, callback, &context, plan.device,
                [plan.relativePath!] as CFArray, since, 0, options) else { return }
            var started = false
            defer {
                if started { FSEventStreamStop(stream) }
                FSEventStreamInvalidate(stream); FSEventStreamRelease(stream)
            }
            let callbacks = DispatchQueue(label: "OrbitDisk.ReuseHistory", qos: .utility)
            FSEventStreamSetDispatchQueue(stream, callbacks)
            guard probe.pending, FSEventStreamStart(stream) else { return }
            started = true
            guard probe.historyCompletion.wait(timeout: deadline) == .success, probe.pending else { return }
            // A history sentinel alone does not prove that latency-buffered live events were delivered.
            FSEventStreamFlushSync(stream)
            probe.finish(plan.rootIdentityIsValid())
        }
        return probe.wait(timeout: timeout)
    }
}

private struct EventOverflow {
    var historyFinished = false
    var identityInvalidated = false
    var latestEventID: UInt64 = 0
    var requiresRefresh = false

    mutating func merge(excluded: [String], paths: [String],
                        flags: [FSEventStreamEventFlags], ids: [UInt64]) {
        for id in ids where id != UInt64(kFSEventStreamEventIdSinceNow) { latestEventID = max(latestEventID, id) }
        if paths.count != flags.count || flags.count != ids.count { requiresRefresh = true }
        for index in flags.indices {
            let flag = flags[index]
            func has(_ value: Int) -> Bool { flag & FSEventStreamEventFlags(value) != 0 }
            if has(kFSEventStreamEventFlagEventIdsWrapped) || has(kFSEventStreamEventFlagRootChanged)
                || has(kFSEventStreamEventFlagMount) || has(kFSEventStreamEventFlagUnmount) {
                identityInvalidated = true
                requiresRefresh = true
            }
            if has(kFSEventStreamEventFlagUserDropped) || has(kFSEventStreamEventFlagKernelDropped) {
                requiresRefresh = true
            }
            if has(kFSEventStreamEventFlagHistoryDone) { historyFinished = true; continue }
            guard index < paths.count, Self.isExcluded(paths[index], by: excluded) else {
                requiresRefresh = true
                continue
            }
        }
    }
    #if ORBIT_TESTING
    static func testExcluded(_ path: String, by exclusions: [String]) -> Bool { isExcluded(path, by: exclusions) }
    #endif

    private static func isExcluded(_ path: String, by exclusions: [String]) -> Bool {
        path.withCString { raw in
            let length = strlen(raw)
            guard length == path.utf8.count, strstr(raw, "/../") == nil,
                  !(length >= 3 && strncmp(raw, "../", 3) == 0),
                  !(length == 2 && strncmp(raw, "..", 2) == 0),
                  !(length >= 3 && strcmp(raw.advanced(by: length - 3), "/..") == 0) else { return false }
            for exclusion in exclusions {
                let match = exclusion.withCString { prefix -> Bool in
                    let count = strlen(prefix)
                    guard count <= length, strncmp(raw, prefix, count) == 0 else { return false }
                    return count == 0 || count == length || (count == 1 && prefix[0] == 47) || raw[count] == 47
                }
                if match { return true }
            }
            return false
        }
    }
}

private final class EventGeneration: @unchecked Sendable {
    private let lock = NSLock()
    private var value = UUID()
    func replace(with value: UUID) { lock.lock(); self.value = value; lock.unlock() }
    func isCurrent(_ value: UUID) -> Bool { lock.lock(); defer { lock.unlock() }; return self.value == value }
}

private struct NormalizedEventBatch: Sendable {
    var directories: Set<String> = []
    var fullRefresh = false
    var identityInvalidated = false
    var historyFinished = false
    var latestEventID: UInt64 = 0

    static func prepare(plan: DiskEventPlan, excluded: [String], paths: [String],
                        flags: [FSEventStreamEventFlags], ids: [UInt64],
                        isCurrent: () -> Bool) -> Self? {
        guard isCurrent() else { return nil }
        var result = Self()
        result.identityInvalidated = !plan.rootIdentityIsValid()
        result.fullRefresh = paths.count != flags.count || flags.count != ids.count || result.identityInvalidated
        var cache = EventPathCache()
        for index in flags.indices {
            if index & 127 == 0, !isCurrent() { return nil }
            let flag = flags[index]
            let has: (Int) -> Bool = { flag & FSEventStreamEventFlags($0) != 0 }
            if index < ids.count, ids[index] != UInt64(kFSEventStreamEventIdSinceNow) {
                result.latestEventID = max(result.latestEventID, ids[index])
            }
            if has(kFSEventStreamEventFlagEventIdsWrapped) || has(kFSEventStreamEventFlagRootChanged) {
                result.fullRefresh = true
                result.identityInvalidated = true
            }
            if has(kFSEventStreamEventFlagUserDropped) || has(kFSEventStreamEventFlagKernelDropped) {
                result.fullRefresh = true
            }
            if has(kFSEventStreamEventFlagHistoryDone) { result.historyFinished = true; continue }
            guard index < paths.count else { result.fullRefresh = true; continue }
            guard let path = plan.normalizedEventPath(paths[index], cache: &cache) else {
                if plan.eventMayAffectRoot(paths[index]) { result.fullRefresh = true }
                continue
            }
            if excluded.contains(where: { DiskEventPlan.contains(path, in: $0) }) { continue }
            if has(kFSEventStreamEventFlagMount) || has(kFSEventStreamEventFlagUnmount) {
                result.fullRefresh = true
                result.identityInvalidated = true
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
            result.directories.insert(DiskEventPlan.contains(directory, in: plan.rootPath) ? directory : plan.rootPath)
        }
        guard isCurrent() else { return nil }
        if !cache.revalidate() { result.fullRefresh = true }
        if !plan.rootIdentityIsValid() {
            result.fullRefresh = true
            result.identityInvalidated = true
        }
        return result
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
    let rawExcludedPaths: [String]
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
        let nativeExcludedPaths = Array(Set(excludedPaths.flatMap { path -> [String] in
            [path, DiskEventPlan.nativeExclusionPath(path)].compactMap { $0 }
        }))
        let rawExcludedPaths = Array(Set(nativeExcludedPaths.flatMap { plan.rawExclusionPaths($0) }))
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
        return DiskEventSetup(plan: plan, excludedPaths: excludedPaths, rawExcludedPaths: rawExcludedPaths, since: since,
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

private struct EventPathCache {
    struct Entry {
        let parent: String
        let nativeParent: String
        let device: dev_t
        let inode: ino_t
    }
    struct Identity: Equatable {
        let device: dev_t
        let inode: ino_t
        let mode: mode_t
    }
    var parents: [Data: Entry] = [:]
    var ancestors: [Data: (path: String, identity: Identity)] = [:]
    var invalidated = false

    mutating func trackAncestors(_ path: String) -> Bool {
        let components = path.split(separator: "/")
        guard components.count <= 128, parents.count < 256,
              ancestors.count + components.count + 1 <= 1024 else { return false }
        var current = ""
        for component in [Substring("")] + components {
            current = component.isEmpty ? "/" : (current == "/" ? "/" + component : current + "/" + component)
            var info = stat()
            guard lstat(current, &info) == 0 else { return false }
            let identity = Identity(device: info.st_dev, inode: info.st_ino, mode: info.st_mode)
            let key = Data(current.utf8)
            if let previous = ancestors[key], previous.identity != identity {
                invalidated = true
                return false
            }
            ancestors[key] = (path: current, identity: identity)
        }
        return true
    }

    func revalidate(path: String) -> Bool {
        var current = ""
        for component in [Substring("")] + path.split(separator: "/") {
            current = component.isEmpty ? "/" : (current == "/" ? "/" + component : current + "/" + component)
            guard let previous = ancestors[Data(current.utf8)] else { return false }
            var info = stat()
            guard lstat(current, &info) == 0,
                  Identity(device: info.st_dev, inode: info.st_ino, mode: info.st_mode) == previous.identity else { return false }
        }
        return true
    }

    func revalidate() -> Bool {
        guard !invalidated else { return false }
        for (_, previous) in ancestors {
            var info = stat()
            guard lstat(previous.path, &info) == 0,
                  Identity(device: info.st_dev, inode: info.st_ino, mode: info.st_mode) == previous.identity else { return false }
        }
        return true
    }
}

private struct DiskEventPlan: Sendable {
    struct Volume: Codable, Equatable {
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
        let bootTraversal: Int?
    }

    let rootPath: String
    let canonicalRoot: String
    let physicalRoot: String
    let mountPath: String
    let relativePath: String?
    let untrackedMounts: [String]
    let device: dev_t
    let rootFileID: UInt64
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
        // Boot helper volumes have no journals; the verified Data volume is part of whole-Mac mapping.
        let supportsReplay = !volumes.isEmpty && volumes.allSatisfy { $0.volumeUUID != nil }
            && (relative != nil ? volumes[0].eventsUUID != nil
                : hostID != nil && volumes.contains { $0.eventsUUID != nil })
        var untrackedMounts = relative == nil && rootPath == "/"
            ? volumes.map(\.mountPath).filter { $0 != "/" } : []
        #if canImport(OrbitCore)
        if rootPath == "/", (try? MappingCoverage.canReach(root: root,
            target: URL(fileURLWithPath: "/System/Volumes/Data"),
            expectedRootIdentity: "\(metadata.st_dev):\(metadata.st_ino)")) == true {
            untrackedMounts.removeAll { $0 == "/System/Volumes/Data" }
        }
        #endif
        let fingerprint = Identity(rootPath: rootPath, canonicalRoot: canonicalRoot,
                                   rootFileID: metadata.st_ino, mode: relative == nil ? "host" : "device",
                                   hostID: hostID, volumes: volumes, bootTraversal: rootPath == "/" ? 2 : nil)
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        let identity = String(decoding: try encoder.encode(fingerprint), as: UTF8.self)
        // This FSEvents API uses the Unix epoch despite naming its argument CFAbsoluteTime.
        let eventID = relative == nil ? FSEventsGetCurrentEventId()
            : FSEventsGetLastEventIdForDeviceBeforeTime(metadata.st_dev, Date().timeIntervalSince1970)
        return DiskEventPlan(rootPath: rootPath, canonicalRoot: canonicalRoot, physicalRoot: physical,
                             mountPath: mount, relativePath: relative, untrackedMounts: untrackedMounts,
                             device: metadata.st_dev, rootFileID: metadata.st_ino,
                             supportsReplay: supportsReplay,
                             checkpoint: DiskChangeCheckpoint(rootPath: rootPath, identity: identity, eventID: eventID))
    }

    func normalizedEventPath(_ raw: String) -> String? {
        var cache = EventPathCache()
        return normalizedEventPath(raw, cache: &cache)
    }

    func normalizedEventPath(_ raw: String, cache: inout EventPathCache) -> String? {
        guard !raw.contains("\0"), !raw.split(separator: "/").contains(".."),
              let separator = raw.lastIndex(of: "/"), separator != raw.index(before: raw.endIndex) else {
            return normalizedEventPathUncached(raw)
        }
        let parent = separator == raw.startIndex ? "/" : String(raw[..<separator])
        let name = String(raw[raw.index(after: separator)...])
        let key = Data(parent.utf8)
        let nativeParent: String
        if relativePath != nil {
            let suffix = parent.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            nativeParent = mountPath == "/" ? "/" + suffix : mountPath + "/" + suffix
        } else { nativeParent = parent }
        var info = stat()
        let usableParent = lstat(nativeParent, &info) == 0 && info.st_mode & S_IFMT == S_IFDIR
        if usableParent, let entry = cache.parents[key], entry.nativeParent == nativeParent,
           entry.device == info.st_dev, entry.inode == info.st_ino {
            return entry.parent == "/" ? "/" + name : entry.parent + "/" + name
        }
        cache.parents.removeValue(forKey: key)
        let tracked = usableParent && cache.trackAncestors(nativeParent)
        let normalized = normalizedEventPathUncached(raw)
        if tracked && !cache.revalidate(path: nativeParent) { cache.invalidated = true }
        guard let normalized else { return nil }
        let suffix = "/" + name
        guard tracked, !cache.invalidated,
              normalized.utf8.suffix(suffix.utf8.count).elementsEqual(suffix.utf8) else { return normalized }
        let normalizedParent = String(normalized.dropLast(suffix.count))
        cache.parents[key] = EventPathCache.Entry(parent: normalizedParent.isEmpty ? "/" : normalizedParent,
            nativeParent: nativeParent, device: info.st_dev, inode: info.st_ino)
        return normalized
    }

    private func normalizedEventPathUncached(_ raw: String) -> String? {
        guard !raw.contains("\0"), !raw.split(separator: "/").contains("..") else { return nil }
        var path: String
        if relativePath != nil {
            path = URL(fileURLWithPath: mountPath).appendingPathComponent(raw).standardizedFileURL.path
            let relative = raw.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            let nativePath = mountPath == "/" ? "/" + relative : mountPath + "/" + relative
            if Self.contains(path, in: physicalRoot) {
                if path != physicalRoot {
                    guard let resolvedRoot = Self.realPath(physicalRoot),
                          Self.resolveEventPath(nativePath, within: resolvedRoot) != nil else { return nil }
                }
                path = rootPath + String(path.dropFirst(physicalRoot.count))
            } else if Self.contains(physicalRoot, in: path) {
                return rootPath
            } else {
                guard let resolvedRoot = Self.realPath(physicalRoot),
                      let resolvedEvent = Self.resolveEventPath(nativePath, within: resolvedRoot),
                      Self.contains(resolvedEvent, in: resolvedRoot) else { return nil }
                path = rootPath + String(resolvedEvent.dropFirst(resolvedRoot.count))
            }
        } else {
            guard raw.hasPrefix("/") else { return nil }
            path = URL(fileURLWithPath: raw).standardizedFileURL.path
            if canonicalRoot == "/", path.hasPrefix("/System/Volumes/Data/") {
                let alias = String(path.dropFirst("/System/Volumes/Data".count))
                if Self.matchesThroughExistingAncestor(path, alias) { path = alias }
            }
            if Self.contains(path, in: canonicalRoot) {
                if path != canonicalRoot {
                    guard let resolvedRoot = Self.realPath(canonicalRoot),
                          Self.resolveEventPath(raw, within: resolvedRoot) != nil else { return nil }
                }
                path = rootPath == "/" ? path : rootPath + String(path.dropFirst(canonicalRoot.count))
            } else if Self.contains(canonicalRoot, in: path) {
                return rootPath
            } else if let physicalRoot = Self.realPath(canonicalRoot),
                      let physicalEvent = Self.resolveEventPath(raw, within: physicalRoot),
                      Self.contains(physicalEvent, in: physicalRoot) {
                let suffix = String(physicalEvent.dropFirst(physicalRoot.count))
                path = rootPath == "/" ? physicalEvent : rootPath + suffix
            } else { return nil }
        }
        return Self.contains(path, in: rootPath) ? path : nil
    }

    private static func realPath(_ path: String) -> String? {
        guard let resolved = path.withCString({ realpath($0, nil) }) else { return nil }
        defer { free(resolved) }
        return String(validatingUTF8: resolved)
    }

    // Preserve the final component: the event can refer to a removed item or a symlink itself.
    // Resolve an existing parent, then append any removed intermediate components lexically.
    #if ORBIT_TESTING
    static func testResolveEventPath(_ raw: String, within root: String) -> String? { resolveEventPath(raw, within: root) }
    #endif

    private static func resolveEventPath(_ raw: String, within root: String) -> String? {
        guard raw.hasPrefix("/") else { return nil }
        let components = raw.split(separator: "/").map(String.init)
        guard !components.isEmpty else { return "/" }
        var prefix = Array(components.dropLast())
        var suffix = [components.last!]
        while true {
            let candidate = "/" + prefix.joined(separator: "/")
            if let resolved = realPath(candidate) {
                guard contains(resolved, in: root) else { return nil }
                return resolved == "/" ? "/" + suffix.joined(separator: "/")
                    : resolved + "/" + suffix.joined(separator: "/")
            }
            guard (errno == ENOENT || errno == ENOTDIR), let last = prefix.popLast() else { return nil }
            suffix.insert(last, at: 0)
        }
    }

    static func nativeExclusionPath(_ path: String) -> String? {
        if path == "/" { return "/" }
        return realPath(path) ?? resolveEventPath(path, within: "/")
    }

    func rawExclusionPaths(_ native: String) -> [String] {
        if relativePath == nil {
            var paths = [native]
            if canonicalRoot == "/", native.hasPrefix("/"), native != "/" {
                let dataPath = "/System/Volumes/Data" + native
                if Self.matchesThroughExistingAncestor(dataPath, native) { paths.append(dataPath) }
            }
            return paths
        }
        let suffix: String
        if Self.contains(native, in: mountPath) {
            suffix = mountPath == "/" ? String(native.dropFirst()) : String(native.dropFirst(mountPath.count)).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        } else {
            guard native.hasPrefix("/") else { return [] }
            let candidate = mountPath + native
            guard Self.matchesThroughExistingAncestor(candidate, native) else { return [] }
            suffix = String(native.dropFirst())
        }
        return suffix.isEmpty ? ["/", ""] : [suffix, "/" + suffix]
    }

    func nativeEventPath(_ raw: String) -> String? {
        guard !raw.contains("\0"), !raw.split(separator: "/").contains("..") else { return nil }
        if relativePath != nil {
            let suffix = raw.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            return mountPath == "/" ? "/" + suffix : mountPath + "/" + suffix
        }
        return raw.hasPrefix("/") ? raw : nil
    }

    func eventMayAffectRoot(_ raw: String) -> Bool {
        guard !raw.contains("\0"), !raw.split(separator: "/").contains("..") else { return true }
        let path: String
        if relativePath != nil {
            let suffix = raw.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            path = mountPath == "/" ? "/" + suffix : mountPath + "/" + suffix
        } else {
            guard raw.hasPrefix("/") else { return true }
            path = raw
        }
        guard let resolvedRoot = Self.realPath(canonicalRoot) else { return true }
        return [canonicalRoot, physicalRoot, resolvedRoot].contains {
            Self.contains(path, in: $0) || Self.contains($0, in: path)
        }
    }

    func rootIdentityIsValid() -> Bool {
        var metadata = stat()
        return lstat(canonicalRoot, &metadata) == 0 && metadata.st_dev == device
            && metadata.st_ino == rootFileID && metadata.st_mode & S_IFMT == S_IFDIR
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

#if ORBIT_TESTING
extension DiskChangeMonitor {
    func testReceive(event: String, id: UInt64 = 0, flag: FSEventStreamEventFlags = 0) {
        receive(paths: [event], flags: [flag], ids: [id])
    }

    func testReceiveBatch(events: [String], id: UInt64) {
        receive(paths: events, flags: Array(repeating: 0, count: events.count), ids: Array(repeating: id, count: events.count))
    }

    func testBlockNormalization(_ semaphore: DispatchSemaphore) {
        normalizationQueue.async { semaphore.wait() }
    }
    var testQueuedNormalizations: Int { queuedNormalizations }
    var testHasOverflow: Bool { overflow != nil }
    var testOverflowRequiresRefresh: Bool { overflow?.requiresRefresh ?? false }

    nonisolated static func testRawExclusions(root: URL, excluded: URL) throws -> [String] {
        let plan = try DiskEventPlan.inspect(root: root)
        let variants = [excluded.path, DiskEventPlan.nativeExclusionPath(excluded.path)].compactMap { $0 }
        return variants.flatMap { plan.rawExclusionPaths($0) }
    }

    nonisolated static func testExclusionPlan(root: URL, excluded: URL) throws -> String {
        let plan = try DiskEventPlan.inspect(root: root)
        let variants = [excluded.path, DiskEventPlan.nativeExclusionPath(excluded.path)].compactMap { $0 }
        return "mount=\(plan.mountPath) relative=\(String(describing: plan.relativePath)) physical=\(plan.physicalRoot) variants=\(variants.flatMap { plan.rawExclusionPaths($0) })"
    }

    nonisolated static func testOverflowExcluded(_ path: String, exclusions: [String]) -> Bool {
        EventOverflow.testExcluded(path, by: exclusions)
    }

    nonisolated static func testCombinedOverflowFlags() -> Bool {
        var overflow = EventOverflow()
        let flags = FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone | kFSEventStreamEventFlagRootChanged)
        overflow.merge(excluded: [], paths: ["/"], flags: [flags], ids: [1])
        return overflow.historyFinished && overflow.identityInvalidated && overflow.requiresRefresh
    }

    nonisolated static func testBatchRefresh(root: URL, event: String, device: Bool = false) throws -> Bool {
        let p = try DiskEventPlan.inspect(root: root)
        let plan = device ? DiskEventPlan(rootPath: p.rootPath, canonicalRoot: p.canonicalRoot,
            physicalRoot: p.canonicalRoot, mountPath: "/", relativePath: "fixture",
            untrackedMounts: [], device: p.device, rootFileID: p.rootFileID,
            supportsReplay: p.supportsReplay, checkpoint: p.checkpoint) : p
        return NormalizedEventBatch.prepare(plan: plan, excluded: [], paths: [event], flags: [0], ids: [1], isCurrent: { true })!.fullRefresh
    }

    nonisolated static func testBatchDirectories(root: URL, event: String) throws -> Set<String> {
        NormalizedEventBatch.prepare(plan: try DiskEventPlan.inspect(root: root), excluded: [],
            paths: [event], flags: [0], ids: [1], isCurrent: { true })!.directories
    }

    nonisolated static func testNormalizeBatch(root: URL, events: [String]) throws -> [String?] {
        let plan = try DiskEventPlan.inspect(root: root)
        var cache = EventPathCache()
        return events.map { plan.normalizedEventPath($0, cache: &cache) }
    }

    nonisolated static func testCacheRace(root: URL, first: String, second: String,
                                          between: () throws -> Void) throws -> Bool {
        let plan = try DiskEventPlan.inspect(root: root)
        var cache = EventPathCache()
        _ = plan.normalizedEventPath(first, cache: &cache)
        try between()
        _ = plan.normalizedEventPath(second, cache: &cache)
        return cache.revalidate()
    }

    nonisolated static func testNormalize(root: URL, event: String) throws -> String? {
        try DiskEventPlan.inspect(root: root).normalizedEventPath(event)
    }

    nonisolated static func testDeviceNormalize(root: URL, event: String, mount: String) throws -> String? {
        let p = try DiskEventPlan.inspect(root: root)
        let devicePlan = DiskEventPlan(rootPath: p.rootPath, canonicalRoot: p.canonicalRoot,
            physicalRoot: p.canonicalRoot, mountPath: mount, relativePath: "fixture",
            untrackedMounts: [], device: p.device, rootFileID: p.rootFileID, supportsReplay: p.supportsReplay, checkpoint: p.checkpoint)
        return devicePlan.normalizedEventPath(event)
    }

    nonisolated static func testCanonicalNormalize(root: URL, event: String, realRoot: String) throws -> String? {
        let p = try DiskEventPlan.inspect(root: root)
        let plan = DiskEventPlan(rootPath: realRoot, canonicalRoot: realRoot,
            physicalRoot: realRoot, mountPath: p.mountPath, relativePath: nil,
            untrackedMounts: [], device: p.device, rootFileID: p.rootFileID,
            supportsReplay: p.supportsReplay, checkpoint: p.checkpoint)
        return plan.normalizedEventPath(event)
    }

    nonisolated static func testResolveEventPath(_ path: String, within root: String) -> String? {
        DiskEventPlan.testResolveEventPath(path, within: root)
    }
}
#endif
