import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
    private let lock = NSLock(), signal = DispatchSemaphore(value: 0)
    private var state = false, used = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return state }
    func pause() { lock.lock(); let block = !used; used = true; if block { state = true }; lock.unlock(); if block { signal.wait() } }
    func release() { signal.signal() }
}
@main struct Test {
 @MainActor static func wait(_ name:String,_ f:()->Bool) async throws {
    for _ in 0..<500 { if f() { return }; try await Task.sleep(nanoseconds:20_000_000) }
    throw NSError(domain:name,code:1)
 }
 @MainActor static func main() async throws {
    let base=URL(fileURLWithPath:CommandLine.arguments[1]), root=base.appendingPathComponent("root"), folder=root.appendingPathComponent("match-folder")
    let fm=FileManager.default;try fm.createDirectory(at:folder,withIntermediateDirectories:true)
    for i in 0..<350 { try Data([1]).write(to:root.appendingPathComponent(String(format:"match-%03d",i))) }
    for i in 0..<20 { try Data([1,2]).write(to:folder.appendingPathComponent(String(format:"match-nested-%02d",i))) }
    let model=DiskModel(cacheDirectory:base.appendingPathComponent("index"),preferences:UserDefaults(suiteName:"saved-search-\(UUID())")!)
    model.scan(root);try await wait("saved map") {!model.isScanning && model.current?.children.count == 351}
    let store = DiskIndexStore(directory: base.appendingPathComponent("index"))
    let saved = store.savedSnapshot(for: root)!
    try await wait("background search preparation") { store.isSearchPrepared(snapshot: saved) }
    model.query="match";try await wait("initial search") {!model.isSearching && model.searchMatchCount == 371}
    let file=root.appendingPathComponent("match-000")
    let wanted=model.current!.children.filter {$0.id == file.path || $0.id == folder.path}
    precondition(wanted.count == 2 && model.collect(wanted))
    try await wait("saved search collection") { !model.isValidatingCollection }
    let gate=Gate();model.testBeforeRefreshSave={gate.pause()}
    model.deleteCollectionPermanently();try await wait("deletion") {!model.isRemoving && !fm.fileExists(atPath:file.path) && !fm.fileExists(atPath:folder.path)}
    try await wait("saved refresh held") {gate.waiting}
    try await wait("projected search") {!model.isSearching && model.searchMatchCount == 349 && model.searchResults.count == 300}
    precondition(!model.searchResults.contains{$0.id == file.path || $0.id == folder.path || $0.id.hasPrefix(folder.path + "/")})
    gate.release();try await wait("refresh complete") {!model.isRefreshing && !model.isSearching && model.searchMatchCount == 349}
    model.cancelScan()
    let switchRoot=base.appendingPathComponent("switch-root"), switchFile=switchRoot.appendingPathComponent("victim-match"), other=base.appendingPathComponent("other-root")
    try fm.createDirectory(at:switchRoot,withIntermediateDirectories:true);try fm.createDirectory(at:other,withIntermediateDirectories:true)
    try Data([1]).write(to:switchFile);try Data([2]).write(to:other.appendingPathComponent("other"))
    let switchCache=base.appendingPathComponent("switch-index"), switchModel=DiskModel(cacheDirectory:switchCache,preferences:UserDefaults(suiteName:"switch-search-\(UUID())")!)
    switchModel.scan(switchRoot);try await wait("switch initial") {!switchModel.isScanning && switchModel.current?.children.count == 1}
    switchModel.query="victim";try await wait("switch search") {!switchModel.isSearching && switchModel.searchResults.count == 1}
    precondition(switchModel.collect([switchModel.current!.children[0]]))
    try await wait("switch collection") { !switchModel.isValidatingCollection }
    var armed=false, switched=false
    let removalObserver=switchModel.$isRemoving.dropFirst().sink { removing in if !removing && !switched { armed=true } }
    let observer=switchModel.objectWillChange.sink {
        if armed && !switched && !switchModel.isRemoving && switchModel.statusMessage?.contains("Deleted") == true {
            armed=false; switched=true; switchModel.cancelScan(); switchModel.scan(other)
        }
    }
    switchModel.deleteCollectionPermanently()
    try await wait("search-notification switch") {switched && switchModel.root?.id == other.path && !switchModel.isScanning}
    precondition(!fm.fileExists(atPath:switchFile.path))
    switchModel.scan(switchRoot);try await wait("removed session restored") {switchModel.root?.id == switchRoot.path && !switchModel.isScanning && !switchModel.isPartialMap && switchModel.root?.fileCount == 0}
    let switchStore=DiskIndexStore(directory:switchCache)
    try await wait("removed session reconciled") {
        guard let saved=switchStore.savedSnapshot(for:switchRoot) else {return false}
        return (try? switchStore.node(path:switchFile.path,snapshot:saved)) == nil
    }
    withExtendedLifetime((removalObserver,observer)) {};switchModel.cancelScan()
    print("PASS: saved search hides deletions with accurate count while reconciliation is gated")
 }
}
