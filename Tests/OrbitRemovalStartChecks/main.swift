import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(), semaphore=DispatchSemaphore(value:0); private var value=false, countValue=0
 var count:Int {lock.lock();defer{lock.unlock()};return countValue}
 var waiting:Bool {lock.lock();defer{lock.unlock()};return value}
 func pause(){lock.lock();value=true;countValue += 1;lock.unlock();semaphore.wait()}
 func release(){semaphore.signal()}
}
enum Mode: String { case publishedSwitch, activitySwitch, sameRootFeedback, publishedForce, publishedForceSwitch, invalidateRoot }
@main struct Check {
 @MainActor static func wait(_ f: () -> Bool) async throws {
  for _ in 0..<1000 {if f(){return};try await Task.sleep(nanoseconds:10_000_000)}
  throw NSError(domain:"timeout",code:1)
 }
 @MainActor static func check(_ mode: Mode, base: URL) async throws {
  let a=base.appendingPathComponent("a"),b=base.appendingPathComponent("b"),fm=FileManager.default
  for dir in [a,b] {try fm.createDirectory(at:dir,withIntermediateDirectories:true);try Data([1]).write(to:dir.appendingPathComponent("file"))}
  let model=DiskModel(cacheDirectory:base.appendingPathComponent("cache"),preferences:UserDefaults(suiteName:"removal-start-\(UUID())")!),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.scan(b);try await wait {model.root?.id==b.path && !model.isScanning}
  model.scan(a);try await wait {model.root?.id==a.path && !model.isScanning}
  guard let item=model.current?.children.first,model.collect([item]) else {throw NSError(domain:"collection setup",code:1)}
  try await wait { !model.isValidatingCollection }
  model.testBeforeRemoval={gate.pause()}
  var armed=true, invoked=false
  func onNotification() {
   guard armed else{return};armed=false;invoked=true
   if mode == .invalidateRoot { model.root = nil }
   else if mode == .publishedForce || mode == .publishedForceSwitch {
    model.testActivateResolved(a, force:true)
    model.deleteCollectionPermanently()
    if mode == .publishedForceSwitch { model.testActivateResolved(b) }
   } else if mode != .sameRootFeedback { model.testActivateResolved(b) }
   model.errorMessage="new error";model.statusMessage="new status"
  }
  let observer: AnyCancellable
  if mode == .activitySwitch {
   observer=model.$isRemovingAcrossSessions.dropFirst().sink { active in if active {onNotification()} }
  } else if mode == .publishedSwitch || mode == .publishedForce || mode == .publishedForceSwitch || mode == .invalidateRoot {
   observer=model.$isRemoving.dropFirst().sink { active in if active {onNotification()} }
  } else { observer=model.objectWillChange.sink { _ in onNotification() } }
  model.deleteCollectionPermanently()
  observer.cancel()
  guard invoked else {throw NSError(domain:"\(mode): observer not invoked",code:1)}
  let expectedRoot = mode == .sameRootFeedback || mode == .publishedForce || mode == .invalidateRoot ? a.path : b.path
  guard model.testActiveSessionRoot()==expectedRoot else {throw NSError(domain:"\(mode): wrong root",code:1)}
  let expectedRemoving = mode == .sameRootFeedback || mode == .publishedForce || mode == .invalidateRoot
  guard model.errorMessage=="new error",model.statusMessage=="new status",model.isRemoving==expectedRemoving else {
   throw NSError(domain:"\(mode): feedback corrupted: error=\(model.errorMessage ?? "nil"), status=\(model.statusMessage ?? "nil"), removing=\(model.isRemoving)",code:1)
  }
  try await wait {gate.waiting}
  if mode != .sameRootFeedback && mode != .publishedForce && mode != .invalidateRoot {
   model.testActivateResolved(a)
   guard model.testActiveSessionRoot()==a.path,model.isRemoving else {throw NSError(domain:"\(mode): A removal lost after return",code:1)}
   model.testActivateResolved(b)
   guard model.testActiveSessionRoot()==b.path,!model.isRemoving else {throw NSError(domain:"\(mode): B incorrectly marked removing",code:1)}
  }
  guard gate.count == 1 else { throw NSError(domain:"\(mode): duplicate removal started",code:1) }
  gate.release();try await wait {!model.isRemovingAcrossSessions}
  guard fm.fileExists(atPath:b.appendingPathComponent("file").path), !fm.fileExists(atPath:a.appendingPathComponent("file").path) else {throw NSError(domain:"\(mode): wrong deletion target",code:1)}
  print("PASS: \(mode.rawValue), correct owner, feedback, and deletion")
 }
 @MainActor static func main() async throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL
  for mode in [Mode.publishedSwitch, .activitySwitch, .sameRootFeedback, .publishedForce, .publishedForceSwitch, .invalidateRoot] { try await check(mode, base:base.appendingPathComponent(mode.rawValue)) }
 }
}
