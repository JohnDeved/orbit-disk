import AppKit
import Darwin
import Foundation
import OrbitCore
import CSQLite
final class StageGate: @unchecked Sendable {
    private let lock=NSLock(), semaphore=DispatchSemaphore(value:0); private var waitingValue=false, used=false
    var waiting: Bool { lock.lock(); defer {lock.unlock()}; return waitingValue }
    func pause() { lock.lock(); waitingValue=true; lock.unlock(); semaphore.wait(); lock.lock(); waitingValue=false; lock.unlock() }
    func pauseOnce() { lock.lock(); if used { lock.unlock(); return }; used=true; lock.unlock(); pause() }
    func release() { semaphore.signal() }
}
final class CancellationResult: @unchecked Sendable {
    private let lock=NSLock(); private var value=false
    func mark() { lock.lock(); value=true; lock.unlock() }
    var cancelled: Bool { lock.lock(); defer {lock.unlock()}; return value }
}
func waitForSignal(_ semaphore: DispatchSemaphore) -> Bool { semaphore.wait(timeout:.now()+2) == .success }
@main struct ResumeModelChecks {
    @MainActor static func wait(_ description: String, _ condition: () -> Bool) async throws {
        for _ in 0..<1000 { if condition() { return }; try await Task.sleep(nanoseconds:20_000_000) }
        throw NSError(domain: description, code: 1)
    }
    @MainActor static func main() async throws {
        let args=CommandLine.arguments
        if args.count == 4 && args[1] == "hold" {
            let root=URL(fileURLWithPath:args[2],isDirectory:true), cache=URL(fileURLWithPath:args[3],isDirectory:true)
            let gate=StageGate(), model=DiskModel(cacheDirectory:cache,preferences:UserDefaults(suiteName:"stage-holder-\(UUID())")!)
            model.testAfterStageAttach={ try? Data().write(to:cache.appendingPathComponent("ready")); gate.pause() }
            model.scan(root,force:true)
            try await wait("holder must remain running") { false }
            return
        }
        if args.count == 5 && args[1] == "child" {
            let root=URL(fileURLWithPath:args[2],isDirectory:true), cache=URL(fileURLWithPath:args[3],isDirectory:true)
            let prefs=UserDefaults(suiteName:"resume-child-\(UUID())")!
            let model=DiskModel(cacheDirectory:cache,preferences:prefs,scanProgressObserver:{ p in
                if p.currentPath == root.path && p.itemCount >= 128 { kill(getpid(),SIGKILL) }
            })
            model.scan(root,force:args[4] == "force")
            try await wait("child never committed") { false }
            return
        }
        let base=URL(fileURLWithPath:args[1],isDirectory:true), fm=FileManager.default
        for savedFirst in [false,true] {
            let root=base.appendingPathComponent(savedFirst ? "saved-root" : "fresh-root",isDirectory:true)
            let cache=base.appendingPathComponent(savedFirst ? "saved-cache" : "fresh-cache",isDirectory:true)
            try fm.createDirectory(at:root,withIntermediateDirectories:true)
            try fm.createDirectory(at:cache,withIntermediateDirectories:true)
            try Data([1]).write(to:root.appendingPathComponent("old"))
            var prior: IndexSnapshot?
            if savedFirst {
                let cp=try DiskChangeMonitor.capture(root:root)
                prior=try DiskIndexStore(directory:cache).save(DiskScanner().scan(root),eventID:cp.eventID,eventIdentity:cp.identity)
            }
            for i in 0..<1200 { try Data([1]).write(to:root.appendingPathComponent("file-\(i)")) }
            let child=Process();child.executableURL=URL(fileURLWithPath:args[0]).standardizedFileURL
            child.arguments=["child",root.path,cache.path,savedFirst ? "force" : "normal"]
            try child.run();child.waitUntilExit()
            guard child.terminationReason == .uncaughtSignal,child.terminationStatus == SIGKILL else { throw NSError(domain:"child not killed after staged chunk",code:1) }
            let gate=StageGate(), prefs=UserDefaults(suiteName:"resume-parent-\(UUID())")!
            let model=DiskModel(cacheDirectory:cache,preferences:prefs)
            model.testAfterStageAttach={gate.pause()}
            model.scan(root)
            if savedFirst {
                try await wait("saved map during resume") { gate.waiting && model.root?.id == root.path && model.current?.children.count == 1 && model.isRefreshing }
                guard model.collect([model.current!.children[0]]) else { throw NSError(domain:"saved collection unavailable",code:1) }
                try await wait("saved collection") { !model.isValidatingCollection }
            } else {
                try await wait("recovered partial map") { gate.waiting && model.root?.id == root.path && (model.current?.children.count ?? 0) >= 128 && model.isPartialMap }
                guard model.collect([model.current!.children[0]]) else { throw NSError(domain:"partial collection unavailable",code:1) }
                try await wait("partial collection") { !model.isValidatingCollection }
            }
            let selected=model.current!.children[0].id
            model.selectedID=selected
            gate.release()
            try await wait("resumed map complete") { !model.isScanning && !model.isRefreshing && !model.isPartialMap && !model.isOpeningFolder && model.root?.fileCount == 1201 }
            guard model.selectedID == selected, model.collection.contains(where:{$0.id == selected}),
                  let saved=DiskIndexStore(directory:cache).savedSnapshot(for:root), saved.id != prior?.id else { throw NSError(domain:"selection or completed snapshot",code:1) }
            model.cancelScan()
            let stageFiles=try fm.contentsOfDirectory(at:cache.appendingPathExtension("staging").appendingPathComponent(".orbit-stages"),includingPropertiesForKeys:nil)
            guard let stageFile=stageFiles.first(where:{$0.pathExtension == "sqlite"}) else { throw NSError(domain:"missing stage after publication",code:1) }
            var stageDB: OpaquePointer?
            guard sqlite3_open_v2(stageFile.path,&stageDB,SQLITE_OPEN_READWRITE,nil) == SQLITE_OK else { throw NSError(domain:"open stage marker",code:1) }
            guard sqlite3_exec(stageDB,"UPDATE sealed SET published=0",nil,nil,nil) == SQLITE_OK else { sqlite3_close(stageDB); throw NSError(domain:"reset stage marker",code:1) }
            sqlite3_close(stageDB)
            try Data([2,3]).write(to:root.appendingPathComponent("later"))
            let store=DiskIndexStore(directory:cache)
            let refreshed=try store.update(snapshot:saved, replacements:[IndexReplacement(path:root.path, result:try DiskScanner().scan(root))], eventID:saved.eventID, eventIdentity:saved.eventIdentity)
            let reopened=DiskModel(cacheDirectory:cache, preferences:UserDefaults(suiteName:"stage-reopen-\(UUID())")!)
            reopened.scan(root)
            try await wait("reopen updated map after stage publication") { reopened.root?.fileCount == 1202 && !reopened.isScanning && !reopened.isRefreshing && !reopened.isOpeningFolder }
            guard store.savedSnapshot(for:root)?.id == refreshed.id, reopened.errorMessage == nil else { throw NSError(domain:"updated snapshot was not reopened",code:1) }
            reopened.rescan()
            try await wait("explicit rescan after staged publication") { reopened.root?.fileCount == 1202 && !reopened.isScanning && !reopened.isRefreshing && !reopened.isOpeningFolder && store.savedSnapshot(for:root)?.id != refreshed.id }
            reopened.cancelScan()
            fputs("PASS: \(savedFirst ? "saved-map-first" : "partial-map-first") app resume after SIGKILL keeps collection and selection\n",stderr)
        }
        do {
            let root=base.appendingPathComponent("purged-root",isDirectory:true), cache=base.appendingPathComponent("purged-cache",isDirectory:true)
            try fm.createDirectory(at:root,withIntermediateDirectories:true); try fm.createDirectory(at:cache,withIntermediateDirectories:true)
            try Data([1]).write(to:root.appendingPathComponent("old"))
            let cp=try DiskChangeMonitor.capture(root:root)
            _ = try DiskIndexStore(directory:cache).save(DiskScanner().scan(root),eventID:cp.eventID,eventIdentity:cp.identity)
            for i in 0..<1200 { try Data([1]).write(to:root.appendingPathComponent("item-\(i)")) }
            let child=Process(); child.executableURL=URL(fileURLWithPath:args[0]).standardizedFileURL; child.arguments=["child",root.path,cache.path,"force"]
            try child.run();child.waitUntilExit()
            guard child.terminationReason == .uncaughtSignal,child.terminationStatus == SIGKILL else { throw NSError(domain:"purge child not killed",code:1) }
            try fm.removeItem(at:cache)
            let gate=StageGate(), model=DiskModel(cacheDirectory:cache,preferences:UserDefaults(suiteName:"purge-app-\(UUID())")!)
            model.testAfterStageAttach={gate.pause()}; model.scan(root)
            try await wait("staged partial map survives cache purge") {gate.waiting && model.isPartialMap && (model.current?.children.count ?? 0) >= 128}
            gate.release()
            try await wait("mapping finishes after cache purge") { !model.isScanning && !model.isRefreshing && !model.isPartialMap && model.root?.fileCount == 1201 }
            guard DiskIndexStore(directory:cache).savedSnapshot(for:root) != nil else { throw NSError(domain:"purged cache not republished",code:1) }
            model.cancelScan()
            fputs("PASS: actual app resumes committed mapping after cache purge\n",stderr)
        }
        let root=base.appendingPathComponent("stop-root",isDirectory:true), cache=base.appendingPathComponent("stop-cache",isDirectory:true)
        try fm.createDirectory(at:root,withIntermediateDirectories:true)
        try fm.createDirectory(at:cache,withIntermediateDirectories:true)
        for i in 0..<1200 { try Data([1]).write(to:root.appendingPathComponent("item-\(i)")) }
        let first=StageGate(), resumed=StageGate()
        let model=DiskModel(cacheDirectory:cache,preferences:UserDefaults(suiteName:"stage-stop-\(UUID())")!)
        model.testScanner=DiskScanner(testBatch:{first.pause()})
        model.scan(root)
        try await wait("partial map before stop") { first.waiting && model.isPartialMap && (model.current?.children.count ?? 0) >= 128 }
        guard let selected=model.current?.children.first, model.collect([selected]) else { throw NSError(domain:"stop collection unavailable",code:1) }
        try await wait("collection before stop") { !model.isValidatingCollection }
        model.selectedID=selected.id
        model.cancelScan(); first.release()
        model.testScanner=DiskScanner()
        model.testAfterStageAttach={resumed.pause()}
        model.rescan()
        try await wait("retained map before resumed work") { resumed.waiting && model.isPartialMap && (model.current?.children.count ?? 0) >= 128 }
        guard model.selectedID == selected.id, model.collection.contains(where:{$0.id == selected.id}) else { throw NSError(domain:"stop restart lost selection or collection",code:1) }
        resumed.release()
        try await wait("stop restart completes") { !model.isScanning && !model.isRefreshing && !model.isPartialMap && model.root?.fileCount == 1200 }
        model.cancelScan()
        fputs("PASS: Stop and Restart retain the staged map, selection, and collection\n",stderr)
        let previous=DiskIndexStore(directory:cache).savedSnapshot(for:root)!
        try Data([9]).write(to:root.appendingPathComponent("after-stop"))
        let publishGate=StageGate()
        DiskIndexStore.testBeforePublish={publishGate.pause()}
        model.testAfterStageAttach={}
        model.rescan()
        try await wait("publication gate") { publishGate.waiting }
        model.cancelScan(); publishGate.release()
        try await Task.sleep(nanoseconds:500_000_000)
        guard DiskIndexStore(directory:cache).savedSnapshot(for:root)?.id == previous.id else { throw NSError(domain:"stopped scan published",code:1) }
        DiskIndexStore.testBeforePublish=nil
        model.rescan()
        try await wait("resume after stopped publication") { !model.isRefreshing && !model.isScanning && model.root?.fileCount == 1201 && DiskIndexStore(directory:cache).savedSnapshot(for:root)?.id != previous.id }
        model.cancelScan()
        fputs("PASS: Stop before publication preserves the old index and restart completes\n",stderr)
        let fallbackRoot=base.appendingPathComponent("fallback-root",isDirectory:true), fallbackCache=base.appendingPathComponent("fallback-cache",isDirectory:true)
        let nested=fallbackRoot.appendingPathComponent("nested",isDirectory:true)
        try fm.createDirectory(at:nested,withIntermediateDirectories:true)
        try fm.createDirectory(at:fallbackCache,withIntermediateDirectories:true)
        for i in 0..<300 { try Data([1,2,3]).write(to:nested.appendingPathComponent("nested-\(i)")) }
        try fm.createSymbolicLink(at:fallbackRoot.appendingPathComponent("escape"),withDestinationURL:root)
        let expected=try DiskScanner(useBulkEnumeration:false).scan(fallbackRoot).root
        let fallbackModel=DiskModel(cacheDirectory:fallbackCache,preferences:UserDefaults(suiteName:"stage-fallback-\(UUID())")!)
        fallbackModel.testScanner=DiskScanner(useBulkEnumeration:false)
        fallbackModel.scan(fallbackRoot)
        try await wait("fallback staged map") { !fallbackModel.isScanning && !fallbackModel.isOpeningFolder && fallbackModel.root?.fileCount == expected.fileCount }
        guard fallbackModel.root?.size == expected.size,
              fallbackModel.current?.children.first(where:{$0.name == "escape"})?.isSymbolicLink == true,
              fallbackModel.errorMessage == nil else { throw NSError(domain:"fallback differed from full scanner",code:1) }
        fallbackModel.cancelScan()
        fputs("PASS: descriptor-relative fallback maps nested files and leaves symlinks untraversed\n",stderr)
        let transitionRoot=base.appendingPathComponent("fallback-transition",isDirectory:true), transitionCache=base.appendingPathComponent("fallback-transition-cache",isDirectory:true)
        try fm.createDirectory(at:transitionRoot,withIntermediateDirectories:true)
        try fm.createDirectory(at:transitionCache,withIntermediateDirectories:true)
        for i in 0..<1200 { try Data([1]).write(to:transitionRoot.appendingPathComponent("item-\(i)")) }
        let transition=DiskModel(cacheDirectory:transitionCache,preferences:UserDefaults(suiteName:"fallback-transition-\(UUID())")!)
        transition.testConfigureStage={ stage in
            stage.configureTestFallbackAfterFirstBuffer { [weak stage] in
                guard let stage, let item = try? stage.view(path:nil, removals:[], cancellation:ScanCancellation())?.folder.children.first else { return }
                try? FileManager.default.removeItem(at:item.url)
            }
        }
        transition.scan(transitionRoot)
        try await wait("partial bulk fallback completes") { !transition.isScanning && !transition.isOpeningFolder && transition.root?.fileCount == 1199 }
        guard transition.current?.children.count == 1199 else { throw NSError(domain:"fallback retained a deleted provisional row",code:1) }
        transition.cancelScan()
        fputs("PASS: deletion between a committed bulk batch and fallback removes the stale row\n",stderr)
        let linksRoot=base.appendingPathComponent("link-order",isDirectory:true), linksCache=base.appendingPathComponent("link-order-cache",isDirectory:true)
        try fm.createDirectory(at:linksRoot,withIntermediateDirectories:true); try fm.createDirectory(at:linksCache,withIntermediateDirectories:true)
        let pairs=[("a/item","a-"),("É","e"),("Z","a1"),("b/item","bA"),("🦊","~")]
        for (first,second) in pairs {
            let one=linksRoot.appendingPathComponent(first), two=linksRoot.appendingPathComponent(second)
            try fm.createDirectory(at:one.deletingLastPathComponent(),withIntermediateDirectories:true)
            try fm.createDirectory(at:two.deletingLastPathComponent(),withIntermediateDirectories:true)
            try Data(repeating:7,count:5000).write(to:one); try fm.linkItem(at:one,to:two)
        }
        let baseline=try DiskScanner().scan(linksRoot).root
        func flatten(_ node:DiskNode) -> [DiskNode] { [node] + node.children.flatMap(flatten) }
        let linkModel=DiskModel(cacheDirectory:linksCache,preferences:UserDefaults(suiteName:"stage-link-order-\(UUID())")!)
        linkModel.scan(linksRoot)
        try await wait("hard link order map") { !linkModel.isScanning && !linkModel.isOpeningFolder && linkModel.root?.fileCount == baseline.fileCount }
        let linkStore=DiskIndexStore(directory:linksCache), linkSnapshot=linkStore.savedSnapshot(for:linksRoot)!
        for expected in flatten(baseline) {
            guard let actual=try linkStore.node(path:expected.id,snapshot:linkSnapshot), actual.allocatedSize == expected.allocatedSize else {
                throw NSError(domain:"hard-link allocation differs for \(expected.id)",code:1)
            }
        }
        let branch=linksRoot.appendingPathComponent("a",isDirectory:true)
        let updated=try linkStore.update(snapshot:linkSnapshot,replacements:[IndexReplacement(path:branch.path,result:try DiskScanner().scan(branch))],eventID:linkSnapshot.eventID,eventIdentity:linkSnapshot.eventIdentity)
        for expected in flatten(baseline) {
            guard let actual=try linkStore.node(path:expected.id,snapshot:updated), actual.allocatedSize == expected.allocatedSize else {
                throw NSError(domain:"incremental hard-link allocation differs for \(expected.id)",code:1)
            }
        }
        linkModel.cancelScan()
        fputs("PASS: staged and incremental hard-link owners match scanner across nested, case and Unicode names\n",stderr)
        let routeRoot=base.appendingPathComponent("route-root",isDirectory:true), routeCache=base.appendingPathComponent("route-cache",isDirectory:true)
        let gone=routeRoot.appendingPathComponent("gone",isDirectory:true), keep=routeRoot.appendingPathComponent("keep",isDirectory:true)
        try fm.createDirectory(at:gone,withIntermediateDirectories:true)
        try fm.createDirectory(at:keep,withIntermediateDirectories:true)
        try fm.createDirectory(at:routeCache,withIntermediateDirectories:true)
        for i in 0..<500 { try Data([1]).write(to:keep.appendingPathComponent("file-\(i)")) }
        let rootListed=StageGate(), missing=StageGate()
        let routeModel=DiskModel(cacheDirectory:routeCache,preferences:UserDefaults(suiteName:"stage-route-\(UUID())")!)
        routeModel.testScanner=DiskScanner(testBatch:{}, testAfterListing:{rootListed.pauseOnce()})
        routeModel.scan(routeRoot)
        try await wait("partial root route") { rootListed.waiting && routeModel.current?.children.contains(where:{$0.id == gone.path}) == true }
        routeModel.open(routeModel.current!.children.first(where:{$0.id == gone.path})!)
        try await wait("open partial folder") { routeModel.current?.id == gone.path }
        routeModel.cancelScan(); rootListed.release()
        try fm.removeItem(at:gone)
        routeModel.testScanner=DiskScanner()
        routeModel.testAfterStageStep={ stage in if stage.confirmsMissingDirectory(gone.path) { missing.pauseOnce() } }
        routeModel.rescan()
        try await wait("missing directory confirmed") { missing.waiting }
        try await wait("missing route returns to root during mapping") { routeModel.current?.id == routeRoot.path && routeModel.isScanning }
        missing.release()
        try await wait("missing route scan completes") { !routeModel.isScanning && !routeModel.isPartialMap && routeModel.root?.fileCount == 500 }
        routeModel.cancelScan()
        fputs("PASS: deleted partial folder returns to root while resumed mapping continues\n",stderr)
        let sharedRoot=base.appendingPathComponent("shared-root",isDirectory:true), sharedCache=base.appendingPathComponent("shared-cache",isDirectory:true)
        try fm.createDirectory(at:sharedRoot,withIntermediateDirectories:true); try fm.createDirectory(at:sharedCache,withIntermediateDirectories:true)
        try Data([1]).write(to:sharedRoot.appendingPathComponent("old"))
        let cp=try DiskChangeMonitor.capture(root:sharedRoot), sharedStore=DiskIndexStore(directory:sharedCache)
        let old=try sharedStore.save(DiskScanner().scan(sharedRoot),eventID:cp.eventID,eventIdentity:cp.identity)
        try Data([2]).write(to:sharedRoot.appendingPathComponent("new"))
        let holder=Process(); holder.executableURL=URL(fileURLWithPath:args[0]).standardizedFileURL
        holder.arguments=["hold",sharedRoot.path,sharedCache.path]; try holder.run()
        try await wait("second process stage lease") { fm.fileExists(atPath:sharedCache.appendingPathComponent("ready").path) }
        let second=DiskModel(cacheDirectory:sharedCache,preferences:UserDefaults(suiteName:"stage-second-\(UUID())")!)
        second.scan(sharedRoot)
        try await wait("saved map while another process owns stage") { second.current?.children.count == 1 && second.errorMessage != nil && !second.isRefreshing }
        guard second.errorMessage?.contains("another Orbit Disk window") == true, sharedStore.savedSnapshot(for:sharedRoot)?.id == old.id else { throw NSError(domain:"busy stage handling",code:1) }
        holder.terminate(); holder.waitUntilExit()
        second.rescan()
        try await wait("retry after other process exits") { !second.isRefreshing && !second.isScanning && second.root?.fileCount == 2 && sharedStore.savedSnapshot(for:sharedRoot)?.id != old.id }
        second.cancelScan()
        fputs("PASS: concurrent process leaves saved map browseable and retry completes after lease release\n",stderr)
        let corruptRoot=base.appendingPathComponent("corrupt-root",isDirectory:true), corruptCache=base.appendingPathComponent("corrupt-cache",isDirectory:true)
        try fm.createDirectory(at:corruptRoot,withIntermediateDirectories:true); try fm.createDirectory(at:corruptCache,withIntermediateDirectories:true)
        try Data([4]).write(to:corruptRoot.appendingPathComponent("file"))
        let corruptCP=try DiskChangeMonitor.capture(root:corruptRoot), corruptStore=DiskIndexStore(directory:corruptCache)
        let corruptSnapshot=try corruptStore.save(DiskScanner().scan(corruptRoot),eventID:corruptCP.eventID,eventIdentity:corruptCP.identity)
        try Data("broken database".utf8).write(to:corruptSnapshot.databaseURL)
        let repair=DiskModel(cacheDirectory:corruptCache,preferences:UserDefaults(suiteName:"corrupt-stage-\(UUID())")!)
        repair.scan(corruptRoot)
        try await wait("rebuild corrupt saved index") { !repair.isScanning && !repair.isOpeningFolder && repair.root?.fileCount == 1 && corruptStore.savedSnapshot(for:corruptRoot) != nil }
        guard repair.errorMessage == nil else { throw NSError(domain:"corrupt cache recovery failed",code:1) }
        try Data("broken again".utf8).write(to:corruptSnapshot.databaseURL)
        repair.rescan()
        try await wait("force rebuild corrupt index") { !repair.isRefreshing && !repair.isScanning && corruptStore.savedSnapshot(for:corruptRoot) != nil }
        repair.cancelScan()
        let outside=base.appendingPathComponent("outside-cache"), bytes=Data("keep me".utf8)
        try bytes.write(to:outside); try fm.removeItem(at:corruptSnapshot.databaseURL)
        try fm.createSymbolicLink(at:corruptSnapshot.databaseURL,withDestinationURL:outside)
        let unsafe=DiskModel(cacheDirectory:corruptCache,preferences:UserDefaults(suiteName:"unsafe-cache-\(UUID())")!)
        unsafe.scan(corruptRoot,force:true)
        try await wait("unsafe cache rejected") { unsafe.errorMessage != nil && !unsafe.isScanning && !unsafe.isOpeningFolder }
        guard try Data(contentsOf:outside) == bytes else { throw NSError(domain:"unsafe cache target changed",code:1) }
        unsafe.cancelScan()
        fputs("PASS: corrupt owned cache rebuilds; symlink replacement is rejected\n",stderr)
        let excludedRoot=base.appendingPathComponent("excluded-root",isDirectory:true)
        let excludedCache=excludedRoot.appendingPathComponent("cache",isDirectory:true), excludedStage=excludedRoot.appendingPathComponent("staging",isDirectory:true)
        try fm.createDirectory(at:excludedRoot,withIntermediateDirectories:true)
        try Data([1]).write(to:excludedRoot.appendingPathComponent("kept"))
        let excludedModel=DiskModel(cacheDirectory:excludedCache,stagingDirectory:excludedStage,preferences:UserDefaults(suiteName:"excluded-stage-\(UUID())")!)
        excludedModel.scan(excludedRoot)
        try await wait("exclude own index and stage") { !excludedModel.isScanning && !excludedModel.isOpeningFolder && excludedModel.root?.fileCount == 1 }
        guard excludedModel.current?.children.map(\.name) == ["kept"] else { throw NSError(domain:"included own stage",code:1) }
        try await Task.sleep(nanoseconds:3_000_000_000)
        try await wait("own-event reconciliation settles") { !excludedModel.isRefreshing && !excludedModel.isScanning }
        let excludedStore=DiskIndexStore(directory:excludedCache)
        let settledID=excludedStore.savedSnapshot(for:excludedRoot)?.id
        try await Task.sleep(nanoseconds:3_000_000_000)
        guard !excludedModel.isRefreshing, excludedModel.root?.fileCount == 1,
              excludedStore.savedSnapshot(for:excludedRoot)?.id == settledID else { throw NSError(domain:"stage feedback refresh",code:1) }
        excludedModel.cancelScan()
        fputs("PASS: mapping excludes its own index and durable stage without a refresh loop\n",stderr)
        let lockFiles=try fm.contentsOfDirectory(at:sharedCache.appendingPathExtension("staging").appendingPathComponent(".orbit-locks"),includingPropertiesForKeys:nil)
        guard let lockFile=lockFiles.first else { throw NSError(domain:"missing root lock",code:1) }
        let lockFD=Darwin.open(lockFile.path,O_RDWR | O_CLOEXEC)
        guard lockFD >= 0, flock(lockFD,LOCK_EX | LOCK_NB) == 0 else { throw NSError(domain:"hold root lock",code:1) }
        let done=DispatchSemaphore(value:0), token=ScanCancellation(), result=CancellationResult()
        let checkpoint=StageCheckpoint(rootPath:cp.rootPath,identity:cp.identity,eventID:cp.eventID)
        DispatchQueue.global().async {
            defer { done.signal() }
            do { _ = try ResumableStage(cache:sharedCache,stagingDirectory:sharedCache.appendingPathExtension("staging"),root:sharedRoot,current:checkpoint,cancellation:token) }
            catch is CancellationError { result.mark() }
            catch {}
        }
        try await Task.sleep(nanoseconds:100_000_000); token.cancel()
        let ended=await Task.detached { waitForSignal(done) }.value
        guard ended, result.cancelled else { flock(lockFD,LOCK_UN); close(lockFD); throw NSError(domain:"cancel held root lease",code:1) }
        flock(lockFD,LOCK_UN); close(lockFD)
        fputs("PASS: cancellation interrupts a held root publication lease\n",stderr)
    }
}
