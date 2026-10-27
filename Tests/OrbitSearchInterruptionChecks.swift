import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 private let lock=NSLock(), signal=DispatchSemaphore(value:0); private var state=false,used=false
 var waiting:Bool {lock.lock();defer{lock.unlock()};return state}
 func pause(){lock.lock();let block = !used;used=true;if block{state=true};lock.unlock();if block{signal.wait()}}
 func release(){signal.signal()}
}
@main struct Test {
 @MainActor static func wait(_ name:String,_ f:()->Bool) async throws {for _ in 0..<500 {if f(){return};try await Task.sleep(nanoseconds:20_000_000)};throw NSError(domain:name,code:1)}
 @MainActor static func main() async throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1]),root=base.appendingPathComponent("root")
  try FileManager.default.createDirectory(at:root,withIntermediateDirectories:true)
  try Data([1]).write(to:root.appendingPathComponent("old-target"));try Data([2]).write(to:root.appendingPathComponent("new-target"))
  let scannerGate=Gate(),searchGate=Gate(), prefs=UserDefaults(suiteName:"interrupt-search-\(UUID())")!
  let model=DiskModel(cacheDirectory:base.appendingPathComponent("index"),preferences:prefs,scanProgressObserver:{if $0.currentPath == root.path && $0.itemCount > 0 {scannerGate.pause()}})
  model.scan(root);try await wait("preview") {scannerGate.waiting && model.current?.children.count == 2}
  model.query="old-target";try await wait("old query") {!model.isSearching && model.searchResults.count == 1}
  model.testBeforePreviewSearch={searchGate.pause()};model.query="new-target"
  try await wait("new query held") {searchGate.waiting && model.isSearching && model.isSearchResultOutdated}
  model.cancelScan();searchGate.release()
  try await wait("stopped search resumes") {!model.isSearching && model.searchResults.first?.name == "new-target"}
  precondition(model.isPartialMap && !model.isSearchResultOutdated)
  scannerGate.release();model.cancelScan()
  let savedRoot=base.appendingPathComponent("saved-root"), savedCache=base.appendingPathComponent("saved-index")
  try FileManager.default.createDirectory(at:savedRoot,withIntermediateDirectories:true)
  try Data([1]).write(to:savedRoot.appendingPathComponent("file"))
  let savedModel=DiskModel(cacheDirectory:savedCache,preferences:prefs)
  savedModel.scan(savedRoot);try await wait("saved map") {!savedModel.isScanning && !savedModel.isPartialMap && savedModel.root?.id == savedRoot.path}
  savedModel.query="no-such-match";try await wait("empty old search") {!savedModel.isSearching && savedModel.searchResults.isEmpty && !savedModel.isSearchResultOutdated}
  let savedSnapshot=DiskIndexStore(directory:savedCache).savedSnapshot(for:savedRoot)!
  try FileManager.default.removeItem(at:savedSnapshot.databaseURL)
  savedModel.testRetrySearch();try await wait("failed same-query search") {!savedModel.isSearching && savedModel.searchFailed}
  precondition(savedModel.searchResults.isEmpty && savedModel.isSearchResultOutdated)
  savedModel.query=""
  precondition(!savedModel.searchFailed)
  var armed=false, checked=false, leaked=false
  let failureObserver=savedModel.objectWillChange.sink {
   if armed && savedModel.query == "no-such-match" && savedModel.isSearching {
    armed=false; savedModel.query="new-query"
    DispatchQueue.main.async { leaked = savedModel.searchFailed; checked = true }
   }
  }
  savedModel.query="no-such-match";armed=true
  try await wait("reentrant failure notification") {checked}
  precondition(!leaked, "Old search failure must not publish into the new query")
  try await wait("failed new search") {!savedModel.isSearching && savedModel.searchFailed}
  precondition(savedModel.searchResults.isEmpty && savedModel.isSearchResultOutdated)
  withExtendedLifetime(failureObserver) {}
  savedModel.cancelScan();print("PASS: interrupted preview search resumes; failed search with empty prior results stays marked incomplete")
 }
}
