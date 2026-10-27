import AppKit
import Combine
import Foundation
import OrbitCore
final class RootGate: @unchecked Sendable {
    let path:String; private let lock=NSLock(); private let gate=DispatchSemaphore(value:0); private var _waiting=false; private var used=false
    init(_ path:String){self.path=path}
    var waiting:Bool{lock.lock(); defer{lock.unlock()}; return _waiting}
    func observe(_ p:ScanProgress){
        lock.lock(); let block = !used && p.currentPath == path && p.itemCount > 0
        if block {used=true;_waiting=true};lock.unlock()
        if block {gate.wait();lock.lock();_waiting=false;lock.unlock()}
    }
    func release(){gate.signal()}
    func arm(){lock.lock();used=false;lock.unlock()}
}
final class ResolutionGate: @unchecked Sendable {
    private let lock=NSLock(), gate=DispatchSemaphore(value:0); private var _waiting=false, used=false
    var waiting:Bool { lock.lock(); defer {lock.unlock()}; return _waiting }
    func pause() { precondition(!Thread.isMainThread); lock.lock();let block = !used;used=true;if block {_waiting=true};lock.unlock();if block {gate.wait();lock.lock();_waiting=false;lock.unlock()} }
    func release() { gate.signal() }
}
final class NthGate: @unchecked Sendable {
    private let lock=NSLock(), gate=DispatchSemaphore(value:0); private let nth:Int; private var count=0; private var _waiting=false
    init(_ nth:Int){self.nth=nth}
    var waiting:Bool {lock.lock();defer{lock.unlock()};return _waiting}
    func pause(){lock.lock();count+=1;let block=count == nth;if block{_waiting=true};lock.unlock();if block{gate.wait();lock.lock();_waiting=false;lock.unlock()}}
    func release(){gate.signal()}
}
@main struct Test {
    @MainActor static func checkMapMembership(_ model: DiskModel) {
        guard let ids = model.mapChildIDs else {
            precondition(model.mapChildIDSet == nil)
            return
        }
        guard let children = model.current?.children else { preconditionFailure("IDs without a current folder") }
        precondition(ids.count == children.count && model.mapChildIDSet == Set(ids))
        for (id, child) in zip(ids, children) {
            precondition(id.utf8.elementsEqual(child.id.utf8), "IDs refer to a different folder revision")
        }
    }
    @MainActor static func wait(_ name:String,_ f:()->Bool) async throws {
        for _ in 0..<500 {if f(){return};try await Task.sleep(nanoseconds:20_000_000)}
        throw NSError(domain:name,code:1)
    }
    @MainActor static func main() async throws {
        let dir=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true)
        let fm=FileManager.default, a=dir.appendingPathComponent("a"), b=dir.appendingPathComponent("b"), sub=a.appendingPathComponent("Downloads")
        try fm.createDirectory(at:sub,withIntermediateDirectories:true);try fm.createDirectory(at:b,withIntermediateDirectories:true)
        try Data([1]).write(to:sub.appendingPathComponent("inside"));try Data([2]).write(to:a.appendingPathComponent("first"));try Data([3, 3, 3]).write(to:b.appendingPathComponent("other"))
        let prefs=UserDefaults(suiteName:"session-check-\(UUID())")!, gate=RootGate(a.path)
        let cache=dir.appendingPathComponent("index"), store=DiskIndexStore(directory:cache)
        let model=DiskModel(cacheDirectory:cache,preferences:prefs,scanProgressObserver:{gate.observe($0)})
        checkMapMembership(model)
        model.scan(a); try await wait("preview a") {gate.waiting && model.current?.children.count == 2}
        checkMapMembership(model)
        precondition(model.collect([model.current!.children.first {$0.name == "first"}!]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        model.scan(sub);try await wait("navigate partial") {model.current?.id == sub.path}
        checkMapMembership(model)
        precondition(store.savedSnapshot(for:sub) == nil)
        model.scan(b);try await wait("scan b") {!model.isScanning && model.root?.id == b.path}
        checkMapMembership(model)
        model.query = "other"; checkMapMembership(model)
        model.sortByName = true; checkMapMembership(model)
        model.query = ""; model.sortByName = false; checkMapMembership(model)
        precondition(gate.waiting && model.collection.isEmpty && model.collectionSize == 0)
        precondition(model.collect([model.current!.children.first!]))
        try await wait("accepted collection") { !model.isValidatingCollection }
        model.scan(a);try await wait("reattach a") {model.root?.id == a.path && model.current?.id == sub.path}
        checkMapMembership(model)
        precondition(gate.waiting && model.isScanning && store.savedSnapshot(for:a) == nil)
        precondition(model.collection.count == 1 && model.collection[0].name == "first" && model.collectionSize == 1)
        model.scan(b);try await wait("reattach b") {!model.isScanning && model.root?.id == b.path}
        checkMapMembership(model)
        precondition(model.collection.count == 1 && model.collection[0].name == "other" && model.collectionSize == 3)
        gate.release(); try await wait("detached a saves") {store.savedSnapshot(for:a) != nil}
        precondition(model.root?.id == b.path)
        try await wait("detached preview released") { !model.testPreviewRetained(a.path) }
        let saved=store.savedSnapshot(for:a)!
        let checkpoint = try DiskChangeMonitor.capture(root:a)
        precondition(saved.eventIdentity == checkpoint.identity)
        model.scan(a); try await wait("saved a") {!model.isScanning && model.root?.id == a.path}
        precondition(store.savedSnapshot(for:a)?.id == saved.id)
        gate.arm(); model.rescan()
        try await wait("refresh a gated") {gate.waiting && model.isRefreshing}
        model.scan(b); try await wait("switch during refresh") {model.root?.id == b.path && !model.isScanning}
        model.scan(a); try await wait("restore refreshing map") {model.root?.id == a.path && model.isRefreshing && !model.isOpeningFolder}
        precondition(model.current?.id == sub.path)
        gate.release(); try await wait("refreshed") {!model.isRefreshing && !model.isOpeningFolder}
        try Data([9,9,9,9]).write(to:a.appendingPathComponent("after-refresh"))
        try await wait("incremental refresh") {model.root?.size == 6 && !model.isRefreshing && !model.isOpeningFolder}
        let updated = store.savedSnapshot(for:a)!
        model.scan(b); try await wait("leave updated map") {model.root?.id == b.path && !model.isScanning}
        model.scan(a); try await wait("restore updated snapshot") {model.root?.id == a.path && !model.isScanning && !model.isOpeningFolder}
        precondition(store.savedSnapshot(for:a)?.id == updated.id && model.root?.size == 6)
        let refreshGate=ResolutionGate();model.testBeforeRefreshSave={refreshGate.pause()}
        try Data([3]).write(to:a.appendingPathComponent("gated-refresh"))
        try await wait("incremental refresh gated") {refreshGate.waiting && model.isRefreshing}
        model.scan(b);try await wait("leave during incremental refresh") {model.root?.id == b.path && !model.isScanning}
        model.scan(a);try await wait("reattach while refreshing") {model.root?.id == a.path && model.isRefreshing && refreshGate.waiting}
        precondition(model.root?.size == 6)
        refreshGate.release();try await wait("reattach after refresh") {model.root?.id == a.path && model.root?.size == 7 && !model.isScanning}
        model.cancelScan()
        let cleared=DiskModel(cacheDirectory:dir.appendingPathComponent("clear-index"),preferences:prefs)
        cleared.scan(b); try await wait("clear fixture") {!cleared.isScanning && cleared.current?.id == b.path}
        checkMapMembership(cleared)
        precondition(cleared.testClearSynthetic())
        precondition(cleared.mapChildIDs == nil && cleared.mapChildIDSet == nil)
        cleared.cancelScan()
        let c=dir.appendingPathComponent("c"), moved=dir.appendingPathComponent("moved")
        try fm.createDirectory(at:c,withIntermediateDirectories:true)
        try Data([4]).write(to:c.appendingPathComponent("old"))
        let cg=RootGate(c.path), m2=DiskModel(cacheDirectory:dir.appendingPathComponent("other-index"),preferences:prefs,scanProgressObserver:{cg.observe($0)})
        m2.scan(c); try await wait("old c gated") {cg.waiting && m2.current?.children.count == 1}
        m2.scan(b); try await wait("other root") {m2.root?.id == b.path && !m2.isScanning}
        try fm.moveItem(at:c,to:moved); try fm.createDirectory(at:c,withIntermediateDirectories:true)
        try Data([5]).write(to:c.appendingPathComponent("new"))
        m2.scan(c); try await Task.sleep(nanoseconds:100_000_000)
        precondition(cg.waiting && m2.isScanning)
        cg.release(); try await wait("replacement saved") {m2.root?.id == c.path && !m2.isScanning}
        precondition(m2.current?.children.map(\.name) == ["new"])
        m2.cancelScan()
        let stop=dir.appendingPathComponent("stop")
        try fm.createDirectory(at:stop,withIntermediateDirectories:true);try Data([1]).write(to:stop.appendingPathComponent("x"))
        let sg=RootGate(stop.path), m3=DiskModel(cacheDirectory:dir.appendingPathComponent("stop-index"),preferences:prefs,scanProgressObserver:{sg.observe($0)})
        m3.scan(stop); try await wait("stop gated") {sg.waiting && m3.isPartialMap}
        m3.cancelScan(); precondition(!m3.isScanning && m3.isPartialMap);sg.release()
        try await Task.sleep(nanoseconds:200_000_000)
        precondition(DiskIndexStore(directory:dir.appendingPathComponent("stop-index")).savedSnapshot(for:stop) == nil)
        let d=dir.appendingPathComponent("d"), inner=d.appendingPathComponent("one/two")
        try fm.createDirectory(at:inner,withIntermediateDirectories:true);try Data([7]).write(to:inner.appendingPathComponent("leaf"))
        let dg=RootGate(d.path), m4=DiskModel(cacheDirectory:dir.appendingPathComponent("nested-index"),preferences:prefs,scanProgressObserver:{dg.observe($0)})
        m4.scan(d); try await wait("ancestor gated") {dg.waiting && m4.current?.children.count == 1}
        m4.scan(inner); precondition(dg.waiting && m4.isScanning)
        m4.scan(b); try await wait("nested switch away") {m4.root?.id == b.path && !m4.isScanning}
        m4.scan(inner); try await wait("reattach nested") {dg.waiting && m4.isScanning}
        dg.release()
        for _ in 0..<200 {
            if m4.root?.id == d.path && m4.current?.id == inner.path && !m4.isScanning && !m4.isOpeningFolder { break }
            try await Task.sleep(nanoseconds: 30_000_000)
        }
        precondition(m4.root?.id == d.path && m4.current?.id == inner.path && !m4.isScanning && !m4.isOpeningFolder)
        precondition(DiskIndexStore(directory:dir.appendingPathComponent("nested-index")).savedSnapshot(for:inner) == nil)
        m4.cancelScan()
        let host=dir.appendingPathComponent("host"), external=dir.appendingPathComponent("external"), child=external.appendingPathComponent("child")
        try fm.createDirectory(at:host,withIntermediateDirectories:true);try fm.createDirectory(at:child,withIntermediateDirectories:true)
        try Data([8]).write(to:host.appendingPathComponent("file"));try Data([9]).write(to:child.appendingPathComponent("x"))
        try fm.createSymbolicLink(at:host.appendingPathComponent("link"),withDestinationURL:external)
        let hg=RootGate(host.path), m5=DiskModel(cacheDirectory:dir.appendingPathComponent("link-index"),preferences:prefs,scanProgressObserver:{hg.observe($0)})
        m5.scan(host);try await wait("host gated") {hg.waiting && m5.current?.children.count == 2}
        precondition(!m5.testReachable(root:host,target:host.appendingPathComponent("link/child")))
        precondition(!m5.testContains(host.path + "-other/x",root:host.path))
        m5.scan(host.appendingPathComponent("link/child"))
        try await wait("symlink scans independently") {m5.root?.id == host.appendingPathComponent("link/child").path && !m5.isScanning}
        precondition(hg.waiting); hg.release(); m5.cancelScan()
        if CommandLine.arguments.count > 2 {
            let mountRoot=URL(fileURLWithPath:CommandLine.arguments[2]), mount=mountRoot.appendingPathComponent("volume")
            precondition(!m5.testReachable(root:mountRoot,target:mount), "Nested mounts must become independent roots")
        }
        let rg=ResolutionGate(), m6=DiskModel(cacheDirectory:dir.appendingPathComponent("resolution-index"),preferences:prefs)
        m6.scan(b);try await wait("resolution baseline") {m6.root?.id == b.path && !m6.isScanning}
        m6.testBeforeResolution = { rg.pause() }
        m6.scan(a);try await wait("resolution gated") {rg.waiting}
        m6.cancelScan();rg.release();try await Task.sleep(nanoseconds:200_000_000)
        precondition(m6.root?.id == b.path && !m6.isScanning && !m6.isOpeningFolder)
        let rem=dir.appendingPathComponent("removal"), victim=rem.appendingPathComponent("victim")
        try fm.createDirectory(at:rem,withIntermediateDirectories:true);try Data([6]).write(to:victim)
        let deletionGate=ResolutionGate(), m7=DiskModel(cacheDirectory:dir.appendingPathComponent("removal-index"),preferences:prefs)
        m7.scan(rem);try await wait("removal initial save") {m7.root?.id == rem.path && !m7.isScanning}
        precondition(m7.collect([m7.current!.children[0]]))
        try await wait("accepted collection") { !m7.isValidatingCollection }
        m7.testBeforeRemoval = {deletionGate.pause()};m7.deleteCollectionPermanently()
        try await wait("removal gated") {deletionGate.waiting && m7.isRemoving}
        m7.scan(b);try await wait("switch during removal") {m7.root?.id == b.path && !m7.isScanning && !m7.isRemoving}
        precondition(m7.collection.isEmpty && m7.isRemovingAcrossSessions)
        m7.scan(rem);try await wait("reattach ongoing removal") {m7.root?.id == rem.path && m7.isRemoving && m7.statusMessage?.contains("Deleting collected items") == true}
        m7.scan(b);try await wait("leave ongoing removal again") {m7.root?.id == b.path && !m7.isRemoving}
        deletionGate.release();try await wait("detached removal") {!fm.fileExists(atPath:victim.path)}
        try await Task.sleep(nanoseconds:100_000_000)
        precondition(m7.root?.id == b.path && m7.collection.isEmpty)
        m7.scan(rem);try await wait("removed map refresh") {m7.root?.id == rem.path && !m7.isScanning && !m7.isRemoving && !m7.isRefreshing && m7.root?.fileCount == 0}
        precondition(m7.collection.isEmpty && m7.collectionSize == 0 && !m7.isRemovingAcrossSessions && m7.statusMessage?.contains("Deleted 1 item") == true)
        m7.cancelScan()
        let multipleA=dir.appendingPathComponent("multiple-a"), multipleB=dir.appendingPathComponent("multiple-b")
        try fm.createDirectory(at:multipleA,withIntermediateDirectories:true);try fm.createDirectory(at:multipleB,withIntermediateDirectories:true)
        let multipleFileA=multipleA.appendingPathComponent("item"), multipleFileB=multipleB.appendingPathComponent("item")
        try Data([1]).write(to:multipleFileA);try Data([2]).write(to:multipleFileB)
        let multipleModel=DiskModel(cacheDirectory:dir.appendingPathComponent("multiple-index"),preferences:prefs)
        multipleModel.scan(multipleA);try await wait("multiple A map") {!multipleModel.isScanning && multipleModel.root?.id == multipleA.path}
        let gateA=ResolutionGate(), gateB=ResolutionGate()
        precondition(multipleModel.collect([multipleModel.current!.children[0]]))
        try await wait("accepted collection") { !multipleModel.isValidatingCollection };multipleModel.testBeforeRemoval={gateA.pause()};multipleModel.deleteCollectionPermanently()
        try await wait("multiple A held") {gateA.waiting && multipleModel.isRemovingAcrossSessions}
        multipleModel.scan(multipleB);try await wait("multiple B map") {!multipleModel.isScanning && multipleModel.root?.id == multipleB.path}
        precondition(!multipleModel.isRemoving && multipleModel.isRemovingAcrossSessions)
        precondition(multipleModel.collect([multipleModel.current!.children[0]]))
        try await wait("accepted collection") { !multipleModel.isValidatingCollection };multipleModel.testBeforeRemoval={gateB.pause()};multipleModel.deleteCollectionPermanently()
        try await wait("multiple B held") {gateB.waiting && multipleModel.isRemovingAcrossSessions}
        gateA.release();try await wait("multiple A completed") {!fm.fileExists(atPath:multipleFileA.path)}
        try await Task.sleep(nanoseconds:100_000_000)
        precondition(multipleModel.isRemovingAcrossSessions && multipleModel.isRemoving)
        gateB.release();try await wait("multiple removals completed") {!multipleModel.isRemovingAcrossSessions && !fm.fileExists(atPath:multipleFileB.path)}
        multipleModel.cancelScan()
        let confirmedRoot=dir.appendingPathComponent("confirmed-removal"), confirmedFile=confirmedRoot.appendingPathComponent("file")
        try fm.createDirectory(at:confirmedRoot,withIntermediateDirectories:true);try Data([1]).write(to:confirmedFile)
        let confirmedModel=DiskModel(cacheDirectory:dir.appendingPathComponent("confirmed-index"),preferences:prefs)
        confirmedModel.scan(confirmedRoot);try await wait("confirmed map") {!confirmedModel.isScanning && confirmedModel.current?.children.count == 1}
        precondition(confirmedModel.collect([confirmedModel.current!.children[0]]))
        try await wait("accepted collection") { !confirmedModel.isValidatingCollection }
        let confirmedObserver=confirmedModel.$isRemovingAcrossSessions.dropFirst().sink { active in
            if active { confirmedModel.collection = [] }
        }
        confirmedModel.deleteCollectionPermanently()
        try await wait("confirmed collection snapshot") {!confirmedModel.isRemovingAcrossSessions && !fm.fileExists(atPath:confirmedFile.path)}
        withExtendedLifetime(confirmedObserver) {};confirmedModel.cancelScan()
        let statusRoot=dir.appendingPathComponent("scan-save-status"), statusFile=statusRoot.appendingPathComponent("file")
        try fm.createDirectory(at:statusRoot,withIntermediateDirectories:true);try Data([1]).write(to:statusFile)
        let statusScan=RootGate(statusRoot.path), statusRemoval=ResolutionGate()
        let statusModel=DiskModel(cacheDirectory:dir.appendingPathComponent("scan-save-status-index"),preferences:prefs,scanProgressObserver:{statusScan.observe($0)})
        statusModel.scan(statusRoot);try await wait("status preview") {statusScan.waiting && statusModel.current?.children.count == 1}
        precondition(statusModel.collect([statusModel.current!.children[0]]))
        try await wait("accepted collection") { !statusModel.isValidatingCollection };statusModel.testBeforeRemoval={statusRemoval.pause()};statusModel.deleteCollectionPermanently()
        try await wait("status removal held") {statusRemoval.waiting && statusModel.isRemoving}
        statusScan.release();try await wait("status scan saved") {!statusModel.isScanning && !statusModel.isPartialMap}
        precondition(statusModel.isRemoving && statusModel.statusMessage?.contains("Deleting collected items") == true)
        statusRemoval.release();try await wait("status removal completed") {!statusModel.isRemovingAcrossSessions && !fm.fileExists(atPath:statusFile.path)}
        statusModel.cancelScan()
        for asyncDiscovery in [false, true] {
            let mode = asyncDiscovery ? "async" : "serial"
            let partial=dir.appendingPathComponent("partial-delete-" + mode), victimFile=partial.appendingPathComponent("pending-file"), victimDir=partial.appendingPathComponent("pending-dir")
            try fm.createDirectory(at:victimDir,withIntermediateDirectories:true)
            try Data([1,2]).write(to:victimFile);try Data([3,4,5]).write(to:victimDir.appendingPathComponent("child"))
            let pg=RootGate(partial.path), partialCache=dir.appendingPathComponent("partial-index-" + mode)
            let mp=DiskModel(cacheDirectory:partialCache,preferences:prefs,scanProgressObserver:{pg.observe($0)})
            mp.testConfigureStage = { $0.testDisableAsyncDiscovery = !asyncDiscovery }
            mp.scan(partial);try await wait("partial deletion fixture") {pg.waiting && mp.current?.children.count == 2}
            if asyncDiscovery {
                try await wait("RAM-only child listing") {
                    mp.current?.children.first(where: { $0.id == victimDir.path })?.scanState == .complete
                }
                precondition(pg.waiting && mp.isScanning, "Discovery must complete the child while its writer remains held")
            } else {
                precondition(mp.current!.children.first{$0.id == victimDir.path}!.scanState != .complete)
            }
            let partialFile = mp.current!.children.first { $0.id == victimFile.path }!
            let fileDrag = mp.beginCollectionDrag([partialFile], label: partialFile.name)!
            let fileToken = fileDrag.data(forType: DiskModel.collectionDragType)!
            mp.endCollectionDrag(fileDrag)
            precondition(mp.collectDrag(data: fileToken), "A file must accept a drop while mapping is held")
            try await wait("partial file collection") { !mp.isValidatingCollection }
            let pendingDirectory = mp.current!.children.first { $0.id == victimDir.path }!
            let staleDrag = mp.beginCollectionDrag([pendingDirectory], label: pendingDirectory.name)!
            let staleToken = staleDrag.data(forType: DiskModel.collectionDragType)!
            mp.scan(b);try await wait("switch away from held partial map") {mp.root?.id == b.path && !mp.isScanning}
            precondition(pg.waiting && !mp.collectDrag(data: staleToken), "A drag must not cross mapped roots")
            mp.scan(partial);try await wait("return to held partial map") {mp.root?.id == partial.path && mp.current?.children.count == 2}
            precondition(pg.waiting && mp.isScanning && mp.collection.map(\.id) == [victimFile.path])
            let returnedDirectory = mp.current!.children.first { $0.id == victimDir.path }!
            let directoryDrag = mp.beginCollectionDrag([returnedDirectory], label: returnedDirectory.name)!
            let directoryToken = directoryDrag.data(forType: DiskModel.collectionDragType)!
            mp.endCollectionDrag(directoryDrag)
            precondition(mp.collectDrag(data: directoryToken), "A discovered folder must accept a drop after switching back")
            try await wait("partial folder collection") { !mp.isValidatingCollection }
            mp.deleteCollectionPermanently()
            try await wait("partial deletion finishes") {!mp.isRemoving && !fm.fileExists(atPath:victimFile.path) && !fm.fileExists(atPath:victimDir.path)}
            precondition(pg.waiting && mp.isScanning)
            try await wait("partial overlay applied") {mp.current?.children.isEmpty == true}
            precondition(mp.root?.fileCount == 0)
            pg.release()
            try await wait("partial reconciliation") {!mp.isScanning && !mp.isRefreshing && !mp.isPartialMap && mp.root?.fileCount == 0}
            let final=DiskIndexStore(directory:partialCache).savedSnapshot(for:partial)!
            let finalFolder = try DiskIndexStore(directory:partialCache).loadFolder(path:partial.path,snapshot:final)
            precondition(finalFolder.children.isEmpty)
            mp.cancelScan()
        }
        let retainedRoot=dir.appendingPathComponent("retained-actions")
        try fm.createDirectory(at:retainedRoot,withIntermediateDirectories:true)
        let retainedFirst=retainedRoot.appendingPathComponent("first"), retainedSecond=retainedRoot.appendingPathComponent("second")
        try Data([1]).write(to:retainedFirst);try Data([2]).write(to:retainedSecond)
        let retained=DiskModel(cacheDirectory:dir.appendingPathComponent("retained-actions-index"),preferences:prefs)
        retained.scan(retainedRoot);try await wait("retained actions map") {!retained.isScanning && retained.current?.children.count == 2}
        precondition(!retained.isPartialMap)
        retained.isScanning = true
        let retainedNodes=retained.current!.children
        precondition(retained.canCollect(retainedNodes[0]) && retained.collect([retainedNodes[0]]),
                     "A visible saved-map item must remain collectable while its session opens")
        try await wait("accepted collection") { !retained.isValidatingCollection }
        let retainedDrag=retained.beginCollectionDrag([retainedNodes[1]],label:retainedNodes[1].name)!
        let retainedToken=retainedDrag.data(forType:DiskModel.collectionDragType)!
        precondition(retained.canAcceptCollectionDrop)
        retained.endCollectionDrag(retainedDrag)
        precondition(retained.collectDrag(data:retainedToken))
        try await wait("retained folder collection") { !retained.isValidatingCollection }
        retained.deleteCollectionPermanently()
        try await wait("delete retained items") {!retained.isRemoving && !fm.fileExists(atPath:retainedFirst.path) && !fm.fileExists(atPath:retainedSecond.path)}
        retained.isScanning = false
        retained.cancelScan()
        for recreate in [false, true] {
            let suffix = recreate ? "recreated" : "deleted"
            let resumedRoot = dir.appendingPathComponent("stopped-removal-" + suffix)
            let nested = resumedRoot.appendingPathComponent("pending-folder")
            let victim = resumedRoot.appendingPathComponent("victim")
            let keeper = dir.appendingPathComponent("old-inode-" + suffix)
            let cache = dir.appendingPathComponent("stopped-removal-index-" + suffix)
            try fm.createDirectory(at:nested,withIntermediateDirectories:true)
            try Data([1]).write(to:victim)
            try Data([2]).write(to:nested.appendingPathComponent("child"))
            if recreate { try fm.linkItem(at:victim,to:keeper) }
            let gate = ResolutionGate()
            let model = DiskModel(cacheDirectory:cache,preferences:prefs)
            model.testMonitorUnavailable = true
            model.testAfterStageStep = { stage in
                guard let view = try? stage.view(path:nil,removals:[],cancellation:ScanCancellation()),
                      view.folder.children.count == 2, !view.isListing else { return }
                gate.pause()
            }
            model.scan(resumedRoot)
            try await wait("stopped removal root listing") {
                gate.waiting && model.isPartialMap && model.current?.children.contains(where:{$0.id == victim.path}) == true
            }
            model.cancelScan(); gate.release()
            try await wait("stopped removal scanner released") {!gate.waiting && !model.isScanning}
            guard let original = model.current?.children.first(where:{$0.id == victim.path}) else { preconditionFailure("Missing mapped victim") }
            precondition(model.collect([original]))
            try await wait("accepted collection") { !model.isValidatingCollection }
            model.deleteCollectionPermanently()
            try await wait("remove while stopped") {!model.isRemoving && !fm.fileExists(atPath:victim.path)}
            try await wait("stopped removal hidden") {model.current?.children.contains(where:{$0.id == victim.path}) == false}
            if recreate { try Data(repeating:7,count:7).write(to:victim) }
            let replayGate = ResolutionGate(), oldIdentity = original.fileIdentity
            model.testAfterStageAttach = {
                // The stage still contains its old committed row before dirty-parent replay.
                // The model must not expose that row while the worker is held here.
                replayGate.pause()
            }
            model.testConfigureStage = { stage in
                guard let node = try? stage.testUnprojectedNode(path: victim.path), node.fileIdentity == oldIdentity else {
                    preconditionFailure("Expected the old row before stage reconciliation")
                }
            }
            model.rescan()
            try await wait("resumed removal gate") {replayGate.waiting && model.isPartialMap}
            precondition(model.current?.children.contains(where:{$0.id == victim.path}) == false,
                         "The stale row must remain hidden while replay is held")
            replayGate.release()
            model.testConfigureStage = { _ in }; model.testAfterStageAttach = {}
            let store = DiskIndexStore(directory:cache)
            try await wait("resumed scan with unavailable events") {
                !model.isScanning && !model.isRefreshing && !model.isPartialMap && model.updatesPaused && store.savedSnapshot(for:resumedRoot) != nil
            }
            let saved = store.savedSnapshot(for:resumedRoot)!
            let reconciled = try store.node(path:victim.path,snapshot:saved)
            precondition(recreate ? reconciled?.fileIdentity != oldIdentity && reconciled?.size == 7 : reconciled == nil,
                         "Stage replay must publish the deletion or the recreated identity")
            model.rescan()
            try await wait("stopped removal reconciled") {
                guard !model.isScanning, !model.isRefreshing, !model.isPartialMap, let children = model.current?.children else { return false }
                return recreate ? children.contains(where:{$0.id == victim.path && $0.size == 7 && $0.fileIdentity != original.fileIdentity})
                    : !children.contains(where:{$0.id == victim.path})
            }
            precondition(model.updatesPaused, "A fresh manual scan must reconcile removals even without event monitoring")
            model.cancelScan()
        }
        let pendingRoot=dir.appendingPathComponent("pending-files")
        try fm.createDirectory(at:pendingRoot,withIntermediateDirectories:true)
        for i in 0..<1200 {try Data([UInt8(i % 251)]).write(to:pendingRoot.appendingPathComponent("file-\(i)"))}
        let pendingGate=ResolutionGate(), pendingCache=dir.appendingPathComponent("pending-index")
        let pendingModel=DiskModel(cacheDirectory:pendingCache,preferences:prefs)
        // This fixture needs an unfinished listing, so its gate must hold discovery too.
        pendingModel.testConfigureStage = { $0.testDisableAsyncDiscovery = true }
        pendingModel.testScanner = DiskScanner(testBatch:{pendingGate.pause()})
        pendingModel.scan(pendingRoot)
        try await wait("pending batch") {pendingGate.waiting && pendingModel.current?.children.isEmpty == false}
        let pendingFile=pendingModel.current!.children.first!
        precondition(!pendingFile.isScanComplete && pendingModel.collect([pendingFile]))
        try await wait("accepted collection") { !pendingModel.isValidatingCollection }
        pendingModel.setMapInteractionActive(true);let revision=pendingModel.mapRevision
        pendingModel.deleteCollectionPermanently()
        try await wait("pending deletion") {!pendingModel.isRemoving && !fm.fileExists(atPath:pendingFile.id)}
        precondition(pendingModel.mapRevision == revision && pendingGate.waiting)
        pendingModel.setMapInteractionActive(false)
        try await wait("pending overlay") {pendingModel.current?.children.contains(where:{$0.id == pendingFile.id}) == false}
        pendingGate.release()
        for _ in 0..<500 {
            if !pendingModel.isScanning && !pendingModel.isRefreshing && !pendingModel.isPartialMap { break }
            try await Task.sleep(nanoseconds:20_000_000)
        }
        precondition(!pendingModel.isScanning && !pendingModel.isRefreshing && !pendingModel.isPartialMap)
        let pendingStore=DiskIndexStore(directory:pendingCache), pendingSaved=pendingStore.savedSnapshot(for:pendingRoot)!
        let pendingNode=try pendingStore.node(path:pendingFile.id,snapshot:pendingSaved)
        precondition(pendingNode == nil && pendingModel.root?.fileCount == 1199)
        pendingModel.cancelScan()
        let saveRoot=dir.appendingPathComponent("save-delete"), saveFile=saveRoot.appendingPathComponent("file")
        try fm.createDirectory(at:saveRoot,withIntermediateDirectories:true);try Data([9]).write(to:saveFile)
        let saveGate=ResolutionGate(), saveCache=dir.appendingPathComponent("save-index")
        DiskIndexStore.testBeforePublish = {saveGate.pause()}
        let saveModel=DiskModel(cacheDirectory:saveCache,preferences:prefs)
        saveModel.scan(saveRoot)
        try await wait("save paused") {saveGate.waiting && saveModel.current?.children.count == 1}
        precondition(saveModel.collect([saveModel.current!.children[0]]))
        try await wait("accepted collection") { !saveModel.isValidatingCollection }
        saveModel.deleteCollectionPermanently()
        try await wait("delete while save paused") {!saveModel.isRemoving && !fm.fileExists(atPath:saveFile.path)}
        precondition(saveGate.waiting)
        try await wait("save overlay") {saveModel.current?.children.isEmpty == true}
        DiskIndexStore.testBeforePublish = nil;saveGate.release()
        try await wait("save reconciliation") {!saveModel.isScanning && !saveModel.isRefreshing && !saveModel.isPartialMap && saveModel.root?.fileCount == 0}
        let saveStore=DiskIndexStore(directory:saveCache), saveSnapshot=saveStore.savedSnapshot(for:saveRoot)!
        let missing=try saveStore.node(path:saveFile.path,snapshot:saveSnapshot)
        precondition(missing == nil);saveModel.cancelScan()
        let nestedRoot=dir.appendingPathComponent("nested-delete"), nestedDir=nestedRoot.appendingPathComponent("branch")
        try fm.createDirectory(at:nestedDir,withIntermediateDirectories:true)
        for i in 0..<1200 {try Data([1]).write(to:nestedDir.appendingPathComponent("child-\(i)"))}
        let nestedGate=NthGate(2), nestedCache=dir.appendingPathComponent("nested-delete-index")
        let nm=DiskModel(cacheDirectory:nestedCache,preferences:prefs)
        nm.testScanner = DiskScanner(testBatch:{nestedGate.pause()});nm.scan(nestedRoot)
        try await wait("nested open while scanning") {nestedGate.waiting && nm.current?.children.count == 1}
        let branch=nm.current!.children[0]
        precondition(nm.collect([branch]))
        try await wait("accepted collection") { !nm.isValidatingCollection };nm.open(branch)
        try await wait("inside folder to remove") {nm.current?.id == nestedDir.path}
        nm.deleteCollectionPermanently()
        try await wait("delete open scanning folder") {!nm.isRemoving && !fm.fileExists(atPath:nestedDir.path)}
        try await wait("deleted folder redirects") {nm.current?.id == nestedRoot.path && nm.current?.children.isEmpty == true}
        precondition(nestedGate.waiting);nestedGate.release()
        for _ in 0..<500 {
            if !nm.isScanning && !nm.isRefreshing && !nm.isPartialMap && nm.root?.fileCount == 0 { break }
            try await Task.sleep(nanoseconds:20_000_000)
        }
        precondition(!nm.isScanning && !nm.isRefreshing && !nm.isPartialMap && nm.root?.fileCount == 0)
        nm.cancelScan()
        let restartRoot=dir.appendingPathComponent("restart-delete"), restartFile=restartRoot.appendingPathComponent("file")
        try fm.createDirectory(at:restartRoot,withIntermediateDirectories:true);try Data([9]).write(to:restartFile)
        let scanGate=RootGate(restartRoot.path), deleteGate=ResolutionGate()
        let restartModel=DiskModel(cacheDirectory:dir.appendingPathComponent("restart-delete-index"),preferences:prefs,scanProgressObserver:{scanGate.observe($0)})
        restartModel.scan(restartRoot);try await wait("restart initial gate") {scanGate.waiting && restartModel.current?.children.count == 1}
        precondition(restartModel.collect([restartModel.current!.children[0]]))
        try await wait("accepted collection") { !restartModel.isValidatingCollection }
        restartModel.testBeforeRemoval = {deleteGate.pause()};restartModel.deleteCollectionPermanently()
        try await wait("restart deletion gate") {deleteGate.waiting && restartModel.isRemoving}
        restartModel.cancelScan();restartModel.rescan();scanGate.release()
        try await wait("restart scan waiting on deletion") {restartModel.isRemoving && restartModel.current?.id == restartRoot.path}
        deleteGate.release();try await wait("restart removal completed") {!restartModel.isRemoving && !fm.fileExists(atPath:restartFile.path)}
        try await wait("restart final reconciliation") {!restartModel.isScanning && !restartModel.isRefreshing && !restartModel.isPartialMap && restartModel.root?.fileCount == 0}
        restartModel.cancelScan()
        let links=dir.appendingPathComponent("links"), original=links.appendingPathComponent("a"), alias=links.appendingPathComponent("b")
        try fm.createDirectory(at:links,withIntermediateDirectories:true);try Data(repeating:8,count:6000).write(to:original);try fm.linkItem(at:original,to:alias)
        let lg=RootGate(links.path), linkCache=dir.appendingPathComponent("links-index")
        let lm=DiskModel(cacheDirectory:linkCache,preferences:prefs,scanProgressObserver:{lg.observe($0)})
        lm.scan(links);try await wait("hardlinks preview") {lg.waiting && lm.current?.children.count == 2}
        precondition(lm.collect([lm.current!.children.first{$0.id == original.path}!]))
        try await wait("accepted collection") { !lm.isValidatingCollection }
        lm.deleteCollectionPermanently();try await wait("hardlink deletion") {!lm.isRemoving && !fm.fileExists(atPath:original.path)}
        precondition(lg.waiting);lg.release()
        try await wait("hardlink reconciliation") {!lm.isScanning && !lm.isRefreshing && !lm.isPartialMap}
        let fresh=try DiskScanner().scan(links)
        precondition(lm.root?.fileCount == fresh.root.fileCount && lm.root?.allocatedSize == fresh.root.allocatedSize && lm.root?.size == fresh.root.size)
        lm.cancelScan()
        let mixed=dir.appendingPathComponent("mixed-removal"), one=mixed.appendingPathComponent("one"), two=mixed.appendingPathComponent("two"), old=mixed.appendingPathComponent("old")
        try fm.createDirectory(at:mixed,withIntermediateDirectories:true);try Data([1]).write(to:one);try Data([2]).write(to:two)
        let mg=RootGate(mixed.path), mm=DiskModel(cacheDirectory:dir.appendingPathComponent("mixed-index"),preferences:prefs,scanProgressObserver:{mg.observe($0)})
        mm.scan(mixed);try await wait("mixed preview") {mg.waiting && mm.current?.children.count == 2}
        precondition(mm.collect(mm.current!.children))
        try await wait("accepted collection") { !mm.isValidatingCollection }
        try fm.moveItem(at:one,to:old);try Data([4,5]).write(to:one)
        let mixedGate=ResolutionGate();mm.testBeforeRemoval={mixedGate.pause()};mm.showCollection=true
        mm.deleteCollectionPermanently();try await wait("mixed deletion held") {mixedGate.waiting && mm.isRemoving}
        mm.showCollection=false;mixedGate.release()
        try await wait("mixed deletion") {!mm.isRemoving && !fm.fileExists(atPath:two.path)}
        precondition(fm.fileExists(atPath:one.path) && mm.collection.count == 1 && mm.collection[0].id == one.path && mm.errorMessage != nil && !mm.showCollection)
        mg.release();try await wait("mixed reconciled") {!mm.isScanning && !mm.isRefreshing && !mm.isPartialMap}
        precondition(mm.current?.children.contains(where:{$0.id == one.path}) == true)
        mm.cancelScan()
        let replacementRoot=dir.appendingPathComponent("replace-active"), replacementDir=replacementRoot.appendingPathComponent("child")
        try fm.createDirectory(at:replacementDir,withIntermediateDirectories:true);try Data([1]).write(to:replacementDir.appendingPathComponent("old"))
        let am=DiskModel(cacheDirectory:dir.appendingPathComponent("replace-active-index"),preferences:prefs)
        am.scan(replacementRoot);try await wait("active root baseline") {am.root?.id == replacementRoot.path && !am.isScanning}
        let oldRoot=dir.appendingPathComponent("replace-active-old")
        try fm.moveItem(at:replacementRoot,to:oldRoot);try fm.createDirectory(at:replacementRoot,withIntermediateDirectories:true)
        try Data([2]).write(to:replacementRoot.appendingPathComponent("new"))
        am.scan(replacementRoot);try await wait("active root replacement") {am.root?.id == replacementRoot.path && !am.isScanning && am.current?.children.map(\.name) == ["new"]}
        am.cancelScan()
        let parent=dir.appendingPathComponent("replace-child"), target=parent.appendingPathComponent("child"), oldTarget=parent.appendingPathComponent("old-child")
        try fm.createDirectory(at:target,withIntermediateDirectories:true);try Data([3]).write(to:target.appendingPathComponent("old"))
        let dm=DiskModel(cacheDirectory:dir.appendingPathComponent("replace-child-index"),preferences:prefs)
        dm.scan(parent);try await wait("child replacement baseline") {dm.root?.id == parent.path && !dm.isScanning}
        try fm.moveItem(at:target,to:oldTarget);try fm.createDirectory(at:target,withIntermediateDirectories:true);try Data([4]).write(to:target.appendingPathComponent("new"))
        dm.scan(target);try await wait("known child replacement") {dm.root?.id == target.path && !dm.isScanning && dm.current?.children.map(\.name) == ["new"]}
        dm.cancelScan()
        let unknownRoot=dir.appendingPathComponent("replace-unknown"), unknown=unknownRoot.appendingPathComponent("one/two"), unknownOld=unknownRoot.appendingPathComponent("one/two-old")
        try fm.createDirectory(at:unknown,withIntermediateDirectories:true);try Data([1]).write(to:unknown.appendingPathComponent("old"))
        let ug=RootGate(unknownRoot.path), um=DiskModel(cacheDirectory:dir.appendingPathComponent("replace-unknown-index"),preferences:prefs,scanProgressObserver:{ug.observe($0)})
        // Keep the destination undiscovered until its identity changes below.
        um.testConfigureStage = { $0.testDisableAsyncDiscovery = true }
        um.scan(unknownRoot);try await wait("unknown root gated") {ug.waiting && um.root?.id == unknownRoot.path}
        um.scan(unknown);try await wait("unknown resolution complete") {um.isOpeningFolder}
        try await Task.sleep(nanoseconds:100_000_000)
        try fm.moveItem(at:unknown,to:unknownOld);try fm.createDirectory(at:unknown,withIntermediateDirectories:true);try Data([2]).write(to:unknown.appendingPathComponent("new"))
        ug.release();try await wait("unknown child replacement") {um.root?.id == unknown.path && !um.isScanning && um.current?.children.map(\.name) == ["new"]}
        um.cancelScan()
        let savedRoot=dir.appendingPathComponent("saved-overlay"), savedA=savedRoot.appendingPathComponent("a"), savedB=savedRoot.appendingPathComponent("b")
        try fm.createDirectory(at:savedRoot,withIntermediateDirectories:true);try Data([1,2]).write(to:savedA);try Data([3,4,5]).write(to:savedB)
        let savedModel=DiskModel(cacheDirectory:dir.appendingPathComponent("saved-overlay-index"),preferences:prefs)
        savedModel.scan(savedRoot);try await wait("saved overlay ready") {savedModel.root?.id == savedRoot.path && !savedModel.isScanning}
        let savedGate=ResolutionGate();savedModel.testBeforeRefreshSave={savedGate.pause()}
        precondition(savedModel.collect([savedModel.current!.children.first{$0.id == savedA.path}!]))
        try await wait("accepted collection") { !savedModel.isValidatingCollection }
        savedModel.deleteCollectionPermanently()
        try await wait("saved removal projected") {!savedModel.isRemoving && savedModel.current?.children.map(\.name) == ["b"] && savedModel.root?.size == 3}
        try await wait("saved refresh gated") {savedGate.waiting && savedModel.isRefreshing}
        savedModel.scan(b);try await wait("saved overlay switch") {savedModel.root?.id == b.path && !savedModel.isScanning}
        savedModel.scan(savedRoot);try await wait("saved overlay reattach") {savedModel.root?.id == savedRoot.path && savedModel.isRefreshing && savedModel.current?.children.map(\.name) == ["b"]}
        precondition(savedModel.root?.size == 3 && savedGate.waiting)
        savedGate.release();try await wait("saved overlay reconciled") {!savedModel.isRefreshing && !savedModel.isScanning && savedModel.root?.size == 3}
        savedModel.cancelScan()
        let deep=dir.appendingPathComponent("deep-indexed"), deepTarget=deep.appendingPathComponent("1/2/3/4/5/target"), deepOld=deep.appendingPathComponent("1/2/3/4/5/target-old")
        try fm.createDirectory(at:deepTarget,withIntermediateDirectories:true);try Data([1]).write(to:deepTarget.appendingPathComponent("old"))
        let deepModel=DiskModel(cacheDirectory:dir.appendingPathComponent("deep-index"),preferences:prefs)
        deepModel.scan(deep);try await wait("deep saved") {deepModel.root?.id == deep.path && !deepModel.isScanning}
        try fm.moveItem(at:deepTarget,to:deepOld);try fm.createDirectory(at:deepTarget,withIntermediateDirectories:true);try Data([2]).write(to:deepTarget.appendingPathComponent("new"))
        deepModel.scan(deepTarget);try await wait("deep replacement") {deepModel.root?.id == deepTarget.path && !deepModel.isScanning && deepModel.current?.children.map(\.name) == ["new"]}
        deepModel.cancelScan()
        let parentRoot=dir.appendingPathComponent("saved-parent-delete"), parentDir=parentRoot.appendingPathComponent("parent"), nestedChild=parentDir.appendingPathComponent("child/1/2/3/4")
        try fm.createDirectory(at:nestedChild,withIntermediateDirectories:true);try Data([1]).write(to:nestedChild.appendingPathComponent("leaf"))
        let parentModel=DiskModel(cacheDirectory:dir.appendingPathComponent("saved-parent-index"),preferences:prefs)
        parentModel.scan(parentRoot);try await wait("saved parent ready") {parentModel.root?.id == parentRoot.path && !parentModel.isScanning}
        let parentNode=parentModel.current!.children[0];precondition(parentModel.collect([parentNode]))
        try await wait("accepted collection") { !parentModel.isValidatingCollection }
        parentModel.scan(nestedChild);try await wait("open requested nested child") {parentModel.current?.id == nestedChild.path && !parentModel.isOpeningFolder && !parentModel.isScanning}
        let parentGate=ResolutionGate();parentModel.testBeforeRefreshSave={parentGate.pause()}
        parentModel.deleteCollectionPermanently();try await wait("deleted ancestor redirected") {parentModel.current?.id == parentRoot.path && parentModel.current?.children.isEmpty == true}
        try await wait("parent refresh gated") {parentGate.waiting};parentGate.release()
        try await wait("parent removal final") {!parentModel.isRefreshing && parentModel.root?.fileCount == 0}
        parentModel.cancelScan()
        let cross=dir.appendingPathComponent("cross-removal"), crossFile=cross.appendingPathComponent("file")
        try fm.createDirectory(at:cross,withIntermediateDirectories:true);try Data([1]).write(to:crossFile)
        let crossScan=RootGate(cross.path), crossDelete=ResolutionGate()
        let cm=DiskModel(cacheDirectory:dir.appendingPathComponent("cross-index"),preferences:prefs,scanProgressObserver:{crossScan.observe($0)})
        cm.scan(cross);try await wait("cross preview") {crossScan.waiting && cm.current?.children.count == 1}
        precondition(cm.collect([cm.current!.children[0]]))
        try await wait("accepted collection") { !cm.isValidatingCollection };cm.testBeforeRemoval={crossDelete.pause()};cm.deleteCollectionPermanently()
        try await wait("cross deletion started") {crossDelete.waiting && cm.isRemoving}
        cm.scan(b);try await wait("cross switched") {cm.root?.id == b.path && !cm.isScanning && !cm.isRemoving}
        crossDelete.release();try await wait("cross removal detached") {!fm.fileExists(atPath:crossFile.path)}
        cm.scan(cross);try await wait("cross reattach overlay") {cm.root?.id == cross.path && cm.current?.children.isEmpty == true && crossScan.waiting}
        precondition(cm.collection.isEmpty);crossScan.release()
        try await wait("cross final") {!cm.isScanning && !cm.isRefreshing && !cm.isPartialMap && cm.root?.fileCount == 0}
        cm.cancelScan()
        let transA=dir.appendingPathComponent("transition-a"), transB=dir.appendingPathComponent("transition-b")
        try fm.createDirectory(at:transA,withIntermediateDirectories:true);try fm.createDirectory(at:transB,withIntermediateDirectories:true)
        let transFileA=transA.appendingPathComponent("a"),transFileB=transB.appendingPathComponent("b")
        try Data([1]).write(to:transFileA);try Data([2]).write(to:transFileB)
        let tm=DiskModel(cacheDirectory:dir.appendingPathComponent("transition-index"),preferences:prefs)
        tm.scan(transB);try await wait("transition b ready") {tm.root?.id == transB.path && !tm.isScanning}
        precondition(tm.collect([tm.current!.children[0]]))
        try await wait("accepted collection") { !tm.isValidatingCollection }
        tm.scan(transA);try await wait("transition a ready") {tm.root?.id == transA.path && !tm.isScanning}
        let oldNode=tm.current!.children[0];precondition(tm.collect([oldNode]))
        try await wait("accepted collection") { !tm.isValidatingCollection }
        let transitionGate=ResolutionGate();tm.testBeforeScannerStart={transitionGate.pause()}
        tm.scan(transB,force:true);try await wait("transition before new preview") {transitionGate.waiting}
        precondition(tm.root?.id == transB.path && !tm.canCollect(oldNode))
        precondition(tm.collect([oldNode]))
        try await wait("reject former root item") { !tm.isValidatingCollection }
        precondition(tm.collection.map(\.id) == [transFileB.path])
        precondition(!tm.isRemoving && fm.fileExists(atPath:transFileA.path) && fm.fileExists(atPath:transFileB.path))
        transitionGate.release();try await wait("transition complete") {tm.root?.id == transB.path && !tm.isScanning && !tm.isRefreshing}
        precondition(tm.collection.map(\.id) == [transFileB.path]);tm.cancelScan()
        let stoppedRoot=dir.appendingPathComponent("stop-removal"), stoppedFile=stoppedRoot.appendingPathComponent("file")
        try fm.createDirectory(at:stoppedRoot,withIntermediateDirectories:true);try Data([1]).write(to:stoppedFile)
        let stoppedScan=RootGate(stoppedRoot.path), stoppedDelete=ResolutionGate()
        let sm=DiskModel(cacheDirectory:dir.appendingPathComponent("stop-removal-index"),preferences:prefs,scanProgressObserver:{stoppedScan.observe($0)})
        sm.scan(stoppedRoot);try await wait("stopped removal preview") {stoppedScan.waiting && sm.current?.children.count == 1}
        precondition(sm.collect([sm.current!.children[0]]))
        try await wait("accepted collection") { !sm.isValidatingCollection };sm.testBeforeRemoval={stoppedDelete.pause()};sm.deleteCollectionPermanently()
        try await wait("stopped removal gated") {stoppedDelete.waiting && sm.isRemoving}
        sm.cancelScan();precondition(sm.isRemoving && sm.isPartialMap && sm.statusMessage?.contains("Deleting collected items") == true)
        stoppedDelete.release();try await wait("stopped removal finishes") {!sm.isRemoving && !fm.fileExists(atPath:stoppedFile.path)}
        try await wait("stopped removal overlay") {sm.current?.children.isEmpty == true}
        stoppedScan.release();sm.rescan()
        try await wait("stopped removal restart") {!sm.isScanning && !sm.isPartialMap && sm.root?.fileCount == 0}
        sm.cancelScan()
        let missingRoot=dir.appendingPathComponent("never-existed")
        let errorModel=DiskModel(cacheDirectory:dir.appendingPathComponent("error-index"),preferences:prefs)
        errorModel.scan(missingRoot);try await wait("missing location failure") {errorModel.errorMessage != nil && !errorModel.isScanning}
        precondition(!errorModel.isOpeningFolder)
        let epochRoot=dir.appendingPathComponent("same-parent-epochs"), epochOne=epochRoot.appendingPathComponent("one"), epochTwo=epochRoot.appendingPathComponent("two")
        try fm.createDirectory(at:epochRoot,withIntermediateDirectories:true);try Data([1]).write(to:epochOne);try Data([2]).write(to:epochTwo)
        let em=DiskModel(cacheDirectory:dir.appendingPathComponent("epochs-index"),preferences:prefs)
        em.scan(epochRoot);try await wait("epoch baseline") {em.root?.id == epochRoot.path && !em.isScanning}
        let epochGate=ResolutionGate();em.testBeforeRefreshSave={epochGate.pause()}
        precondition(em.collect([em.current!.children.first{$0.id==epochOne.path}!]))
        try await wait("accepted collection") { !em.isValidatingCollection }
        em.deleteCollectionPermanently();try await wait("epoch first deletion") {!em.isRemoving && !fm.fileExists(atPath:epochOne.path)}
        try await wait("epoch first refresh gated") {epochGate.waiting && em.isRefreshing}
        let secondNode=em.current!.children.first{$0.id==epochTwo.path}!
        precondition(em.collect([secondNode]))
        try await wait("accepted collection") { !em.isValidatingCollection };em.deleteCollectionPermanently()
        try await wait("epoch second deletion") {!em.isRemoving && !fm.fileExists(atPath:epochTwo.path)}
        try await wait("epoch second overlay") {em.current?.children.isEmpty == true}
        precondition(epochGate.waiting);epochGate.release()
        let epochStore=DiskIndexStore(directory:dir.appendingPathComponent("epochs-index"))
        try await wait("epoch final reconciliation") {
            guard let saved = epochStore.savedSnapshot(for:epochRoot) else { return false }
            return (try? epochStore.loadFolder(path:epochRoot.path,snapshot:saved).children.isEmpty) == true
        }
        let epochSaved=epochStore.savedSnapshot(for:epochRoot)!
        let epochFolder=try epochStore.loadFolder(path:epochRoot.path,snapshot:epochSaved);precondition(epochFolder.children.isEmpty)
        em.cancelScan()
        let fullRoot=dir.appendingPathComponent("full-scan-epochs"), fullOne=fullRoot.appendingPathComponent("one"), fullTwo=fullRoot.appendingPathComponent("two")
        try fm.createDirectory(at:fullRoot,withIntermediateDirectories:true);try Data([1]).write(to:fullOne);try Data([2]).write(to:fullTwo)
        let fullCache=dir.appendingPathComponent("full-epochs-index"), fullModel=DiskModel(cacheDirectory:fullCache,preferences:prefs)
        fullModel.scan(fullRoot);try await wait("full epoch baseline") {!fullModel.isScanning && fullModel.current?.children.count == 2}
        precondition(fullModel.collect([fullModel.current!.children.first{$0.id == fullOne.path}!]))
        try await wait("accepted collection") { !fullModel.isValidatingCollection }
        fullModel.deleteCollectionPermanently();try await wait("full epoch first removal") {!fullModel.isRemoving && !fm.fileExists(atPath:fullOne.path)}
        let fullPublishGate=ResolutionGate(), fullRefreshGate=ResolutionGate()
        DiskIndexStore.testBeforePublish={fullPublishGate.pause()};fullModel.testBeforeRefreshSave={fullRefreshGate.pause()}
        fullModel.rescan();try await wait("full epoch save held") {fullPublishGate.waiting}
        precondition(fullModel.collect([fullModel.current!.children.first{$0.id == fullTwo.path}!]))
        try await wait("accepted collection") { !fullModel.isValidatingCollection }
        fullModel.deleteCollectionPermanently();try await wait("full epoch second removal") {!fullModel.isRemoving && !fm.fileExists(atPath:fullTwo.path)}
        DiskIndexStore.testBeforePublish=nil;fullPublishGate.release()
        try await wait("full epoch later refresh held") {fullRefreshGate.waiting}
        precondition(fullModel.current?.children.isEmpty == true)
        fullRefreshGate.release()
        let fullStore=DiskIndexStore(directory:fullCache)
        try await wait("full epoch reconciled") {
            guard let saved=fullStore.savedSnapshot(for:fullRoot) else {return false}
            return (try? fullStore.loadFolder(path:fullRoot.path,snapshot:saved).children.isEmpty) == true
        }
        fullModel.cancelScan()
        let replacedRoot=dir.appendingPathComponent("removed-recreated"), removedParent=replacedRoot.appendingPathComponent("parent"), replacedChild=removedParent.appendingPathComponent("child")
        try fm.createDirectory(at:replacedChild,withIntermediateDirectories:true)
        try Data([1]).write(to:replacedChild.appendingPathComponent("old"))
        let rm=DiskModel(cacheDirectory:dir.appendingPathComponent("recreated-index"),preferences:prefs)
        rm.scan(replacedRoot);try await wait("recreated baseline") {!rm.isScanning && rm.current?.id == replacedRoot.path}
        let replacedGate=ResolutionGate();rm.testBeforeRefreshSave={replacedGate.pause()}
        precondition(rm.collect([rm.current!.children.first{$0.id == removedParent.path}!]))
        try await wait("accepted collection") { !rm.isValidatingCollection }
        rm.deleteCollectionPermanently();try await wait("recreated removal") {!rm.isRemoving && !fm.fileExists(atPath:removedParent.path)}
        try await wait("recreated refresh held") {replacedGate.waiting}
        try fm.createDirectory(at:replacedChild,withIntermediateDirectories:true)
        try Data([2]).write(to:replacedChild.appendingPathComponent("new"))
        rm.scan(replacedChild);try await wait("recreated selected child") {rm.root?.id == replacedChild.path && !rm.isScanning && rm.current?.children.map(\.name) == ["new"]}
        replacedGate.release();rm.cancelScan()
        // Collection validation belongs to its root, even while another root is visible.
        let queueA=dir.appendingPathComponent("queue-a"), queueB=dir.appendingPathComponent("queue-b")
        try fm.createDirectory(at:queueA,withIntermediateDirectories:true); try fm.createDirectory(at:queueB,withIntermediateDirectories:true)
        for name in ["one", "two", "three"] { try Data([1]).write(to:queueA.appendingPathComponent(name)) }
        try Data([2]).write(to:queueB.appendingPathComponent("other"))
        let qm=DiskModel(cacheDirectory:dir.appendingPathComponent("queue-index"),preferences:prefs)
        qm.scan(queueA); try await wait("queue A") {qm.root?.id == queueA.path && !qm.isScanning}
        let queueOne=qm.current!.children.first{$0.name == "one"}!, queueTwo=qm.current!.children.first{$0.name == "two"}!
        precondition(qm.collect([queueOne]))
        try await wait("accepted collection") { !qm.isValidatingCollection }
        let switchGate=ResolutionGate(); qm.testCollectionPrepare={switchGate.pause()}
        precondition(qm.testCollectInBackground([queueTwo])); try await wait("queue switch held") {switchGate.waiting && qm.isValidatingCollection}
        qm.scan(queueB); try await wait("queue B") {qm.root?.id == queueB.path && !qm.isScanning}
        precondition(qm.collection.isEmpty && !qm.isValidatingCollection)
        precondition(qm.collect([qm.current!.children[0]]))
        try await wait("accepted collection") { !qm.isValidatingCollection }; let bIDs=qm.collection.map(\.id)
        qm.scan(queueA); try await wait("queue A returned") {qm.root?.id == queueA.path && !qm.isScanning && qm.isValidatingCollection}
        precondition(qm.collection.map(\.id) == [queueOne.id])
        qm.collection = []
        qm.statusMessage="newer status"; qm.errorMessage="newer error"
        switchGate.release(); try await wait("queue rebase") {!qm.isValidatingCollection && qm.collection.map(\.id) == [queueTwo.id]}
        precondition(qm.statusMessage == "newer status" && qm.errorMessage == "newer error")
        qm.scan(queueB); try await wait("queue B retained") {qm.root?.id == queueB.path && !qm.isScanning}
        precondition(qm.collection.map(\.id) == bIDs)
        qm.scan(queueA); try await wait("queue A resume") {qm.root?.id == queueA.path && !qm.isScanning}
        let refreshGate2=ResolutionGate(); qm.testCollectionPrepare={refreshGate2.pause()}
        precondition(qm.testCollectInBackground([queueOne])); try await wait("queue refresh held") {refreshGate2.waiting}
        qm.rescan(); try await wait("queue refresh replacement") {!qm.isRefreshing && !qm.isScanning && qm.root?.id == queueA.path}
        precondition(qm.isValidatingCollection)
        refreshGate2.release(); try await wait("queue refresh complete") {!qm.isValidatingCollection && Set(qm.collection.map(\.id)) == Set([queueOne.id,queueTwo.id])}
        qm.collection=[]
        let cancelGate=ResolutionGate(); qm.testCollectionPrepare={cancelGate.pause()}
        precondition(qm.testCollectInBackground([queueOne])); try await wait("queue cancel held") {cancelGate.waiting}
        qm.testCancelCollectionValidation(); cancelGate.release()
        try await Task.sleep(nanoseconds:200_000_000)
        precondition(qm.collection.isEmpty && !qm.isValidatingCollection)
        // Pausing the map does not cancel an already accepted collection operation.
        let pauseGate=ResolutionGate(); qm.testCollectionPrepare={pauseGate.pause()}
        precondition(qm.testCollectInBackground([queueTwo])); try await wait("queue pause held") {pauseGate.waiting}
        qm.cancelScan(); precondition(qm.isValidatingCollection)
        pauseGate.release(); try await wait("queue paused scan finishes collection") {!qm.isValidatingCollection && qm.collection.map(\.id) == [queueTwo.id]}
        qm.collection=[]
        // Subscribers must see the committed snapshot after the background identity check.
        let publishGate=ResolutionGate(); qm.testCollectionPrepare={publishGate.pause()}
        var publishedCollection=false
        let collectionObserver=qm.objectWillChange.sink {
            if qm.collection.map(\.id) == [queueOne.id] { publishedCollection=true }
        }
        precondition(qm.testCollectInBackground([queueOne])); try await wait("queue publication held") {publishGate.waiting}
        publishGate.release(); try await wait("queue publication notification") {publishedCollection}
        precondition(qm.collection.map(\.id) == [queueOne.id]); withExtendedLifetime(collectionObserver) {}
        qm.cancelScan()
        // An observer can move the root during the pre-publication notification.
        let moveRoot=dir.appendingPathComponent("queue-moved-root"), oldMoveRoot=dir.appendingPathComponent("queue-moved-old")
        try fm.createDirectory(at:moveRoot,withIntermediateDirectories:true)
        try Data([1]).write(to:moveRoot.appendingPathComponent("file"))
        let moveModel=DiskModel(cacheDirectory:dir.appendingPathComponent("queue-moved-index"),preferences:prefs)
        moveModel.scan(moveRoot); try await wait("queue move ready") {moveModel.root?.id == moveRoot.path && !moveModel.isScanning}
        let moveGate=ResolutionGate(); moveModel.testCollectionPrepare={moveGate.pause()}
        precondition(moveModel.testCollectInBackground([moveModel.current!.children[0]]))
        try await wait("queue move held") {moveGate.waiting}
        var moveArmed=false, rootWasMoved=false
        let moveObserver=moveModel.objectWillChange.sink {
            if moveArmed && !rootWasMoved && moveModel.isValidatingCollection {
                rootWasMoved=true; try! fm.moveItem(at:moveRoot,to:oldMoveRoot)
                try! fm.createDirectory(at:moveRoot,withIntermediateDirectories:true)
            }
        }
        moveArmed=true; moveGate.release()
        try await wait("queue changed root rejected") {rootWasMoved && !moveModel.isValidatingCollection}
        precondition(moveModel.collection.isEmpty)
        withExtendedLifetime(moveObserver) {}; moveModel.cancelScan()
        // A newer status must not hide a distinct rejection from a background job.
        let feedbackRoot=dir.appendingPathComponent("queue-feedback"), valid=feedbackRoot.appendingPathComponent("valid"), stale=feedbackRoot.appendingPathComponent("stale")
        try fm.createDirectory(at:feedbackRoot,withIntermediateDirectories:true)
        try Data([1]).write(to:valid); try Data([2]).write(to:stale)
        let feedbackModel=DiskModel(cacheDirectory:dir.appendingPathComponent("queue-feedback-index"),preferences:prefs)
        feedbackModel.scan(feedbackRoot); try await wait("queue feedback ready") {feedbackModel.root?.id == feedbackRoot.path && !feedbackModel.isScanning}
        let validNode=feedbackModel.current!.children.first{$0.id == valid.path}!, staleNode=feedbackModel.current!.children.first{$0.id == stale.path}!
        try fm.removeItem(at:stale)
        let feedbackGate=ResolutionGate(); feedbackModel.testCollectionPrepare={feedbackGate.pause()}
        precondition(feedbackModel.testCollectInBackground([validNode,staleNode]))
        try await wait("queue feedback held") {feedbackGate.waiting}
        feedbackModel.statusMessage="newer status"
        feedbackModel.scan(queueB); try await wait("queue feedback away") {feedbackModel.root?.id == queueB.path && !feedbackModel.isScanning}
        feedbackGate.release()
        try await Task.sleep(nanoseconds:200_000_000)
        feedbackModel.scan(feedbackRoot)
        try await wait("queue feedback return") {feedbackModel.root?.id == feedbackRoot.path && !feedbackModel.isScanning && !feedbackModel.isValidatingCollection}
        precondition(feedbackModel.collection.map(\.id) == [valid.path] && feedbackModel.errorMessage != nil)
        feedbackModel.cancelScan()
        print("real model session checks passed")
    }
}
