import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(), semaphore=DispatchSemaphore(value:0); private var value=false
 var waiting:Bool {lock.lock();defer{lock.unlock()};return value}
 func pause(){lock.lock();value=true;lock.unlock();semaphore.wait()}
 func release(){semaphore.signal()}
}
@main struct Check {
 @MainActor static func wait(_ f: () -> Bool) async throws {
  for _ in 0..<1000 {if f(){return};try await Task.sleep(nanoseconds:10_000_000)}
  throw NSError(domain:"timeout",code:1)
 }
 @MainActor static func main() async throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL,a=base.appendingPathComponent("a"),b=base.appendingPathComponent("b"),fm=FileManager.default
  for dir in [a,b] {try fm.createDirectory(at:dir,withIntermediateDirectories:true);try Data([1]).write(to:dir.appendingPathComponent("file"))}
  let model=DiskModel(cacheDirectory:base.appendingPathComponent("cache"),preferences:UserDefaults(suiteName:"removal-finish-\(UUID())")!),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.scan(b);try await wait {model.root?.id==b.path && !model.isScanning}
  model.scan(a);try await wait {model.root?.id==a.path && !model.isScanning}
  guard let item=model.current?.children.first,model.collect([item]) else {throw NSError(domain:"collection setup",code:1)}
  try await wait { !model.isValidatingCollection }
  model.testBeforeRemoval={gate.pause()}
  model.deleteCollectionPermanently()
  try await wait {gate.waiting}
  var armed=true, switched=false
  let observer=model.$isRemoving.dropFirst().sink { removing in
   guard armed && !removing else{return};armed=false;switched=true
   model.testActivateResolved(b)
   model.errorMessage="B error";model.statusMessage="B status"
  }
  var observedError: String?, observedStatus: String?, observedRemoving = true
  let endObserver=model.$isRemovingAcrossSessions.dropFirst().sink { active in
   guard !active else { return }
   observedError=model.errorMessage;observedStatus=model.statusMessage;observedRemoving=model.isRemoving
  }
  gate.release();try await wait {switched && !model.isRemovingAcrossSessions}
  observer.cancel();endObserver.cancel()
  guard model.testActiveSessionRoot()==b.path else {throw NSError(domain:"did not switch to B",code:1)}
  guard observedError=="B error",observedStatus=="B status",!observedRemoving else {
   throw NSError(domain:"A completion overwrote B: error=\(observedError ?? "nil"), status=\(observedStatus ?? "nil"), removing=\(observedRemoving)",code:1)
  }
  model.testActivateResolved(a)
  guard !model.isRemoving else {throw NSError(domain:"completed A was incorrectly restored as removing",code:1)}
  print("PASS: A completion preserves B feedback and completes A")
 }
}
