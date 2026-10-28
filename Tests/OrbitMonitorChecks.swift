import Darwin
import CoreServices
import Foundation

private func check(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() { throw NSError(domain: message, code: 1) }
}
private func real(_ path: String) throws -> String {
    guard let result = path.withCString({ realpath($0, nil) }) else { throw NSError(domain: "realpath", code: Int(errno)) }
    defer { free(result) }
    return String(cString: result)
}
@main struct MonitorChecks {
    @MainActor static func main() async throws {
        try check(DiskChangeMonitor.testCombinedOverflowFlags(), "combined flags lost invalidation")
        try check(DiskChangeMonitor.testOverflowExcluded("/root/cache/file", exclusions: ["/root/cache"]), "excluded descendant missed")
        try check(!DiskChangeMonitor.testOverflowExcluded("/root/cache-sibling/file", exclusions: ["/root/cache"]), "excluded sibling matched")
        try check(!DiskChangeMonitor.testOverflowExcluded("/root/cache/../other", exclusions: ["/root/cache"]), "excluded dot-dot matched")
        try check(!DiskChangeMonitor.testOverflowExcluded("/root/cache\0/other", exclusions: ["/root/cache"]), "excluded NUL matched")
        try check(DiskChangeMonitor.testOverflowExcluded("relative/file", exclusions: [""]), "device root exclusion missed")
        let fm = FileManager.default
        let bootRoot = URL(fileURLWithPath: "/"), boot = try DiskChangeMonitor.capture(root: URL(fileURLWithPath: "/"))
        var oldBoot = try JSONSerialization.jsonObject(with: Data(boot.identity.utf8)) as! [String: Any]
        try check(oldBoot.removeValue(forKey: "bootTraversal") as? Int == 2, "boot traversal fingerprint missing")
        let oldBootIdentity = String(decoding: try JSONSerialization.data(withJSONObject: oldBoot, options: [.sortedKeys]), as: UTF8.self)
        let bootMonitor = try DiskChangeMonitor(root: bootRoot)
        var bootRefresh = false
        let bootReplay = try await bootMonitor.start(sinceEventID: boot.eventID, eventIdentity: oldBootIdentity) { batch in
            if batch.fullRefreshRequired && batch.directoryPaths.contains("/") && batch.checkpoint.identity == boot.identity { bootRefresh = true }
        }
        bootMonitor.stop()
        try check(!bootReplay && bootRefresh, "old root scope did not automatically request full reconciliation")
        let home = fm.homeDirectoryForCurrentUser
        let homeCache = home.appendingPathComponent("Library/Caches", isDirectory: true)
        if fm.fileExists(atPath: homeCache.path) {
            let homePrefixes = try DiskChangeMonitor.testRawExclusions(root: home, excluded: homeCache)
            let homeRelative = String(homeCache.path.dropFirst()) + "/index.sqlite"
            let homeExcluded = DiskChangeMonitor.testOverflowExcluded(homeRelative, exclusions: homePrefixes)
                || DiskChangeMonitor.testOverflowExcluded(homeCache.path + "/index.sqlite", exclusions: homePrefixes)
            try check(homeExcluded, "home cache exclusion missed")
            let hostPrefixes = try DiskChangeMonitor.testRawExclusions(root: URL(fileURLWithPath: "/"), excluded: homeCache)
            let dataAlias = "/System/Volumes/Data" + homeCache.path
            var homeInfo = stat(), dataInfo = stat()
            if stat(homeCache.path, &homeInfo) == 0, stat(dataAlias, &dataInfo) == 0,
               homeInfo.st_dev == dataInfo.st_dev, homeInfo.st_ino == dataInfo.st_ino {
                try check(DiskChangeMonitor.testOverflowExcluded(dataAlias + "/index.sqlite", exclusions: hostPrefixes), "host Data cache exclusion missed")
            }
        }
        let base = fm.temporaryDirectory.appendingPathComponent("orbit-monitor-check-\(UUID().uuidString)", isDirectory: true)
        defer { try? fm.removeItem(at: base) }
        let root = base.appendingPathComponent("root", isDirectory: true)
        let sibling = base.appendingPathComponent("root-sibling", isDirectory: true)
        let outside = base.appendingPathComponent("outside", isDirectory: true)
        try fm.createDirectory(at: root, withIntermediateDirectories: true)
        try fm.createDirectory(at: sibling, withIntermediateDirectories: true)
        try fm.createDirectory(at: outside, withIntermediateDirectories: true)
        let realRoot = try real(root.path)
        let canonical = URL(fileURLWithPath: realRoot, isDirectory: true)
        let removed = root.appendingPathComponent("deleted").path
        let rawRemoved = realRoot + "/deleted"
        let norm = try DiskChangeMonitor.testNormalize(root: root, event: rawRemoved)
        try check(norm == removed, "deleted alias mapping")
        let canonicalNorm = try DiskChangeMonitor.testNormalize(root: canonical, event: rawRemoved)
        try check(canonicalNorm == canonical.standardizedFileURL.appendingPathComponent("deleted").path, "deleted canonical mapping")
        let nested = try DiskChangeMonitor.testNormalize(root: root, event: realRoot + "/missing/child")
        try check(nested == root.appendingPathComponent("missing/child").path, "removed ancestor mapping")
        let siblingNorm = try DiskChangeMonitor.testNormalize(root: root, event: try real(sibling.path) + "/deleted")
        try check(siblingNorm == nil, "sibling escaped boundary")
        let deviceRemoved = try DiskChangeMonitor.testDeviceNormalize(root: root, event: String(rawRemoved.dropFirst()), mount: "/")
        try check(deviceRemoved == removed, "device-relative deleted alias mapping")
        let deviceSibling = try DiskChangeMonitor.testDeviceNormalize(root: root, event: String((try real(sibling.path) + "/deleted").dropFirst()), mount: "/")
        try check(deviceSibling == nil, "device-relative sibling escaped boundary")
        let dotdot = try DiskChangeMonitor.testNormalize(root: root, event: realRoot + "/../outside/missing")
        try check(dotdot == nil, "dot-dot escaped boundary")
        try fm.createSymbolicLink(at: root.appendingPathComponent("escape"), withDestinationURL: outside)
        let escaped = try DiskChangeMonitor.testNormalize(root: root, event: realRoot + "/escape/missing")
        try check(escaped == nil, "symlink escaped boundary")
        let canonicalEscaped = try DiskChangeMonitor.testCanonicalNormalize(root: root, event: realRoot + "/escape/missing", realRoot: realRoot)
        try check(canonicalEscaped == nil, "canonical symlink escaped boundary")
        let linkEvent = try DiskChangeMonitor.testCanonicalNormalize(root: root, event: realRoot + "/escape", realRoot: realRoot)
        try check(linkEvent == realRoot + "/escape", "symlink item should be observed")
        let deviceEscaped = try DiskChangeMonitor.testDeviceNormalize(root: root, event: String((realRoot + "/escape/missing").dropFirst()), mount: "/")
        try check(deviceEscaped == nil, "device-relative symlink escaped boundary")
        let raceRoot = base.appendingPathComponent("race-root", isDirectory: true)
        try fm.createDirectory(at: raceRoot, withIntermediateDirectories: true)
        let observedRoot = try real(raceRoot.path)
        try fm.removeItem(at: raceRoot)
        let afterRemoval = DiskChangeMonitor.testResolveEventPath(observedRoot + "/gone", within: observedRoot)
        try check(afterRemoval == nil, "vanished root accepted")
        let cacheParent = root.appendingPathComponent("cache-parent", isDirectory: true)
        try fm.createDirectory(at: cacheParent.appendingPathComponent("child"), withIntermediateDirectories: true)
        let cacheRoot = try real(cacheParent.appendingPathComponent("child").path)
        let cacheStayedValid = try DiskChangeMonitor.testCacheRace(root: root, first: cacheRoot + "/one", second: cacheRoot + "/two") {
            try fm.moveItem(at: cacheParent, to: outside.appendingPathComponent("moved"))
            try fm.createSymbolicLink(at: cacheParent, withDestinationURL: outside.appendingPathComponent("moved"))
        }
        try check(!cacheStayedValid, "moved ancestor did not invalidate cache")
        let siblingRaw = try real(sibling.path) + "/missing"
        let hostSiblingRefresh = try DiskChangeMonitor.testBatchRefresh(root: root, event: siblingRaw)
        let deviceSiblingRefresh = try DiskChangeMonitor.testBatchRefresh(root: root, event: String(siblingRaw.dropFirst()), device: true)
        let ambiguousRefresh = try DiskChangeMonitor.testBatchRefresh(root: root, event: realRoot + "/escape/missing")
        try check(!hostSiblingRefresh, "host sibling forced full refresh")
        try check(!deviceSiblingRefresh, "device sibling forced full refresh")
        try check(ambiguousRefresh, "ambiguous descendant was not refreshed")
        let checkpointAlias = try DiskChangeMonitor.capture(root: root)
        let checkpointCanonical = try DiskChangeMonitor.capture(root: canonical)
        let aliasMonitor = try DiskChangeMonitor(root: root)
        let canonicalMonitor = try DiskChangeMonitor(root: canonical)
        var aliasCount = 0, canonicalCount = 0, histories = 0
        let replayA = try await aliasMonitor.start(sinceEventID: checkpointAlias.eventID, eventIdentity: checkpointAlias.identity) { batch in
            if batch.historyFinished { histories += 1 }
            if batch.directoryPaths.contains(root.path) { aliasCount += 1 }
        }
        let replayB = try await canonicalMonitor.start(sinceEventID: checkpointCanonical.eventID, eventIdentity: checkpointCanonical.identity) { batch in
            if batch.historyFinished { histories += 1 }
            if batch.directoryPaths.contains(canonical.standardizedFileURL.path) { canonicalCount += 1 }
        }
        try check(replayA && replayB, "history unavailable")
        for _ in 0..<100 { if histories >= 2 { break }; try await Task.sleep(nanoseconds: 100_000_000) }
        try check(histories >= 2, "history timeout")
        try await Task.sleep(nanoseconds: 800_000_000)
        let file = root.appendingPathComponent("event-file")
        let renamed = root.appendingPathComponent("renamed")
        for action in 0..<3 {
            let beforeA = aliasCount, beforeB = canonicalCount
            switch action {
            case 0: try Data([1]).write(to: file)
            case 1: try fm.moveItem(at: file, to: renamed)
            default: try fm.removeItem(at: renamed)
            }
            for _ in 0..<100 {
                if aliasCount > beforeA && canonicalCount > beforeB { break }
                try await Task.sleep(nanoseconds: 100_000_000)
            }
            try check(aliasCount > beforeA && canonicalCount > beforeB, "create/rename/delete event timeout \(action): \(aliasCount),\(canonicalCount)")
        }
        aliasMonitor.stop(); canonicalMonitor.stop()
        let volatileRoot = base.appendingPathComponent("volatile", isDirectory: true)
        try fm.createDirectory(at: volatileRoot, withIntermediateDirectories: true)
        let volatileCheckpoint = try DiskChangeMonitor.capture(root: volatileRoot)
        let volatileMonitor = try DiskChangeMonitor(root: volatileRoot)
        var replacedRootRefresh = false
        _ = try await volatileMonitor.start(sinceEventID: volatileCheckpoint.eventID, eventIdentity: volatileCheckpoint.identity) { batch in
            if batch.fullRefreshRequired && batch.checkpoint.identity.isEmpty { replacedRootRefresh = true }
        }
        try fm.moveItem(at: volatileRoot, to: base.appendingPathComponent("old-volatile"))
        try fm.createSymbolicLink(at: volatileRoot, withDestinationURL: base.appendingPathComponent("old-volatile"))
        volatileMonitor.testReceive(event: volatileRoot.appendingPathComponent("changed").path)
        for _ in 0..<100 { if replacedRootRefresh { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(replacedRootRefresh, "root replacement did not invalidate checkpoint")
        volatileMonitor.stop()
        let orderedRoot = base.appendingPathComponent("ordered", isDirectory: true)
        let oldDir = orderedRoot.appendingPathComponent("old", isDirectory: true)
        let newDir = orderedRoot.appendingPathComponent("new", isDirectory: true)
        try fm.createDirectory(at: oldDir, withIntermediateDirectories: true)
        try fm.createDirectory(at: newDir, withIntermediateDirectories: true)
        let orderedCheckpoint = try DiskChangeMonitor.capture(root: orderedRoot)
        let ordered = try DiskChangeMonitor(root: orderedRoot)
        var delivered: [(Set<String>, UInt64)] = []
        var orderedHistory = false
        var fullRefreshIDs: [UInt64] = []
        var overflowHistory = false
        _ = try await ordered.start(sinceEventID: orderedCheckpoint.eventID, eventIdentity: orderedCheckpoint.identity) { batch in
            if batch.historyFinished { orderedHistory = true }
            if batch.fullRefreshRequired { fullRefreshIDs.append(batch.checkpoint.eventID); overflowHistory = batch.historyFinished }
            if !batch.directoryPaths.isEmpty { delivered.append((batch.directoryPaths, batch.checkpoint.eventID)) }
        }
        for _ in 0..<100 { if orderedHistory { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(orderedHistory, "ordered monitor history timeout")
        delivered.removeAll()
        let testID = orderedCheckpoint.eventID + 1_000_000
        ordered.testReceive(event: oldDir.appendingPathComponent("a").path, id: testID)
        ordered.testReceive(event: newDir.appendingPathComponent("b").path, id: testID + 1)
        for _ in 0..<100 { if delivered.count >= 2 { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(delivered.count >= 2 && delivered[0].0.contains(oldDir.path) && delivered[1].0.contains(newDir.path)
                  && delivered[0].1 == testID && delivered[1].1 == testID + 1, "batch order or watermark changed")
        let burstGate = DispatchSemaphore(value: 0)
        ordered.testBlockNormalization(burstGate)
        for index in 0..<20 {
            let flag = index == 19 ? FSEventStreamEventFlags(kFSEventStreamEventFlagHistoryDone) : 0
            ordered.testReceive(event: oldDir.appendingPathComponent("burst-\(index)").path,
                                id: testID + UInt64(100 + index), flag: flag)
        }
        try check(ordered.testQueuedNormalizations == 8 && ordered.testHasOverflow, "batch queue did not bound burst")
        burstGate.signal()
        for _ in 0..<200 { if fullRefreshIDs.contains(testID + 119) { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(fullRefreshIDs.contains(testID + 119) && overflowHistory, "overflow lost watermark or history")
        delivered.removeAll()
        let many = (0..<10_000).map { oldDir.appendingPathComponent("item-\($0)").path }
        let started = ProcessInfo.processInfo.systemUptime
        ordered.testReceiveBatch(events: many, id: testID + 1_000_000)
        let enqueueMillis = (ProcessInfo.processInfo.systemUptime - started) * 1_000
        for _ in 0..<500 { if delivered.contains(where: { $0.1 == testID + 1_000_000 }) { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(delivered.contains(where: { $0.1 == testID + 1_000_000 }), "large batch was not delivered")
        print("Main actor enqueue for 10,000 events: \(String(format: "%.2f", enqueueMillis)) ms")
        let gate = DispatchSemaphore(value: 0)
        ordered.testBlockNormalization(gate)
        ordered.testReceive(event: oldDir.appendingPathComponent("stale").path, id: testID + 2)
        ordered.stop()
        delivered.removeAll()
        _ = try await ordered.start(sinceEventID: orderedCheckpoint.eventID, eventIdentity: orderedCheckpoint.identity) { batch in
            if !batch.directoryPaths.isEmpty { delivered.append((batch.directoryPaths, batch.checkpoint.eventID)) }
        }
        ordered.testReceive(event: newDir.appendingPathComponent("current").path, id: testID + 3)
        gate.signal()
        for _ in 0..<100 { if delivered.contains(where: { $0.0.contains(newDir.path) && $0.1 == testID + 3 }) { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(delivered.contains(where: { $0.0.contains(newDir.path) && $0.1 == testID + 3 }) && !delivered.contains(where: { $0.1 == testID + 2 }), "stale generation delivered")
        ordered.stop()

        let excludeRoot = base.appendingPathComponent("exclude-root", isDirectory: true)
        let cache = excludeRoot.appendingPathComponent("cache", isDirectory: true)
        try fm.createDirectory(at: cache, withIntermediateDirectories: true)
        let excludedCheckpoint = try DiskChangeMonitor.capture(root: excludeRoot)
        let excludedMonitor = try DiskChangeMonitor(root: excludeRoot, excluding: [cache])
        var excludedHistory = false
        var excludedChanges = 0
        _ = try await excludedMonitor.start(sinceEventID: excludedCheckpoint.eventID, eventIdentity: excludedCheckpoint.identity) { batch in
            if batch.historyFinished { excludedHistory = true }
            if !batch.directoryPaths.isEmpty || batch.fullRefreshRequired { excludedChanges += 1 }
        }
        for _ in 0..<100 { if excludedHistory { break }; try await Task.sleep(nanoseconds: 10_000_000) }
        try check(excludedHistory, "excluded monitor history timeout")
        try await Task.sleep(nanoseconds: 1_000_000_000)
        excludedChanges = 0
        for round in 0..<3 {
            let excludedGate = DispatchSemaphore(value: 0)
            excludedMonitor.testBlockNormalization(excludedGate)
            for index in 0..<8 {
                excludedMonitor.testReceive(event: cache.appendingPathComponent("queued-\(round)-\(index)").path,
                                            id: excludedCheckpoint.eventID + UInt64(round * 100 + index + 1))
            }
            for index in 0..<100 {
                try Data("\(index)".utf8).write(to: cache.appendingPathComponent("index-\(round)-\(index)"))
            }
            for _ in 0..<100 { if excludedMonitor.testHasOverflow { break }; try await Task.sleep(nanoseconds: 50_000_000) }
            try check(excludedMonitor.testHasOverflow, "excluded real writes did not reach overflow, round \(round)")
            try check(!excludedMonitor.testOverflowRequiresRefresh, "excluded real writes forced burst refresh, round \(round)")
            if round == 0 {
                let excludedMany = (0..<10_000).map { cache.appendingPathComponent("burst-\($0)").path }
                let overflowStarted = ProcessInfo.processInfo.systemUptime
                excludedMonitor.testReceiveBatch(events: excludedMany, id: excludedCheckpoint.eventID + 10_000)
                let overflowMillis = (ProcessInfo.processInfo.systemUptime - overflowStarted) * 1_000
                print("Main actor overflow merge for 10,000 excluded events: \(String(format: "%.2f", overflowMillis)) ms")
                try check(!excludedMonitor.testOverflowRequiresRefresh, "excluded batch forced refresh")
            }
            excludedGate.signal()
            for _ in 0..<200 { if excludedMonitor.testQueuedNormalizations == 0 && !excludedMonitor.testHasOverflow { break }; try await Task.sleep(nanoseconds: 10_000_000) }
            try check(excludedMonitor.testQueuedNormalizations == 0 && !excludedMonitor.testHasOverflow && excludedChanges == 0, "excluded write feedback loop, round \(round)")
        }
        try Data([1]).write(to: excludeRoot.appendingPathComponent("outside-cache"))
        for _ in 0..<100 { if excludedChanges > 0 { break }; try await Task.sleep(nanoseconds: 50_000_000) }
        try check(excludedChanges > 0, "non-excluded event not delivered")
        excludedMonitor.stop()
        print("PASS: real FSEvents create/rename/delete for alias and canonical roots; deleted descendants, sibling, traversal and symlink escape checks")
    }
}
