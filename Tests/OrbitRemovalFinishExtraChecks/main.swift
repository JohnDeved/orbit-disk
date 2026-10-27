import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(), semaphore=DispatchSemaphore(value:0); private var hits=0
 var waiting:Bool {lock.lock();defer{lock.unlock()};return hits>0}
 func pauseOnce(){lock.lock();hits += 1;let first=hits==1;lock.unlock();if first {semaphore.wait()}}
 func release(){semaphore.signal()}
}
@main struct Check {
 @MainActor static func wait(_ f: () -> Bool) async throws {for _ in 0..<1000 {if f(){return};try await Task.sleep(nanoseconds:10_000_000)};throw NSError(domain:"timeout",code:1)}
 @MainActor static func create(_ base: URL) async throws -> (DiskModel,URL,URL) {
  let fm=FileManager.default,a=base.appendingPathComponent("a"),b=base.appendingPathComponent("b")
  for dir in [a,b] {try fm.createDirectory(at:dir,withIntermediateDirectories:true);try Data([1]).write(to:dir.appendingPathComponent("file"))}
  try Data([2]).write(to:a.appendingPathComponent("second"))
  let model=DiskModel(cacheDirectory:base.appendingPathComponent("cache"),preferences:UserDefaults(suiteName:"finish-extra-\(UUID())")!)
  model.scan(b);try await wait {model.root?.id==b.path && !model.isScanning}
  model.scan(a);try await wait {model.root?.id==a.path && !model.isScanning}
  guard let item=model.current?.children.first,model.collect([item]) else {throw NSError(domain:"collection setup",code:1)}
  try await wait { !model.isValidatingCollection }
  return (model,a,b)
 }
 @MainActor static func replacement(_ base:URL) async throws {
  let (model,a,b)=try await create(base),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.testBeforeRemovalMerge={gate.pauseOnce()}
  model.deleteCollectionPermanently();try await wait {gate.waiting}
  model.testActivateResolved(a,force:true)
  guard model.isRemoving else {throw NSError(domain:"refresh lost removal",code:1)}
  model.deleteCollectionPermanently()
  model.testActivateResolved(b);model.testActivateResolved(a)
  guard model.isRemoving else {throw NSError(domain:"return lost removal",code:1)}
  gate.release();try await wait {!model.isRemovingAcrossSessions}
  guard !model.isRemoving,model.collection.isEmpty,(model.testRemovalEpoch ?? 0)>0 else {throw NSError(domain:"replacement missed completion",code:1)}
  print("PASS: replacement during overlay rebuild preserves and finishes removal")
 }
 @MainActor static func invalidate(_ base:URL) async throws {
  let (model,_,_)=try await create(base),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.testBeforeRemoval={gate.pauseOnce()};model.deleteCollectionPermanently();try await wait {gate.waiting}
  var armed=true,errorAtEnd:String?,statusAtEnd:String?
  let observer=model.$isRemoving.dropFirst().sink { removing in
   guard armed && !removing else{return};armed=false
   model.root=nil;model.errorMessage="new error";model.statusMessage="new status"
  }
  let end=model.$isRemovingAcrossSessions.dropFirst().sink { active in if !active {errorAtEnd=model.errorMessage;statusAtEnd=model.statusMessage} }
  gate.release();try await wait {!model.isRemovingAcrossSessions};observer.cancel();end.cancel()
  guard !armed,errorAtEnd=="new error",statusAtEnd=="new status" else {throw NSError(domain:"root invalidation overwrote feedback",code:1)}
  print("PASS: invalidating displayed root preserves later feedback")
 }
 @MainActor static func feedbackSwitch(_ base:URL) async throws {
  let (model,_,b)=try await create(base),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.testBeforeRemoval={gate.pauseOnce()};model.deleteCollectionPermanently();try await wait {gate.waiting}
  var armed=true,errorAtEnd:String?,statusAtEnd:String?,pausedAtEnd=false
  let observer=model.objectWillChange.sink { _ in
   guard armed && !model.isRemoving else{return};armed=false
   model.testActivateResolved(b);model.errorMessage="B error";model.statusMessage="B status";model.updatesPaused=true
  }
  let end=model.$isRemovingAcrossSessions.dropFirst().sink { active in
   if !active {errorAtEnd=model.errorMessage;statusAtEnd=model.statusMessage;pausedAtEnd=model.updatesPaused}
  }
  gate.release();try await wait {!model.isRemovingAcrossSessions};observer.cancel();end.cancel()
  guard !armed,errorAtEnd=="B error",statusAtEnd=="B status",pausedAtEnd else {throw NSError(domain:"completion feedback switch overwrote B",code:1)}
  print("PASS: feedback notification switch preserves B update state")
 }
 @MainActor static func monitorSwitch(_ base:URL) async throws {
  let (model,_,b)=try await create(base),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.testBeforeRemoval={gate.pauseOnce()};model.deleteCollectionPermanently();try await wait {gate.waiting}
  model.testStopMonitor();model.testMonitorUnavailable=true
  var armed=true,errorAtEnd:String?,statusAtEnd:String?,pausedAtEnd=true
  let observer=model.objectWillChange.sink { _ in
   guard armed && model.statusMessage == "Deleted 1 item permanently." else {return};armed=false
   model.testActivateResolved(b);model.errorMessage="B error";model.statusMessage="B status";model.updatesPaused=false
  }
  let end=model.$isRemovingAcrossSessions.dropFirst().sink { active in
   if !active {errorAtEnd=model.errorMessage;statusAtEnd=model.statusMessage;pausedAtEnd=model.updatesPaused}
  }
  gate.release();try await wait {!model.isRemovingAcrossSessions};observer.cancel();end.cancel()
  guard !armed,errorAtEnd=="B error",statusAtEnd=="B status",!pausedAtEnd else {throw NSError(domain:"monitor failure overwrote B",code:1)}
  print("PASS: monitor failure notification preserves B feedback")
 }
 @MainActor static func monitorInvalidate(_ base:URL) async throws {
  let (model,_,_)=try await create(base),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.testBeforeRemoval={gate.pauseOnce()};model.deleteCollectionPermanently();try await wait {gate.waiting}
  model.testStopMonitor();model.testMonitorUnavailable=true
  var armed=true,statusAtEnd:String?,pausedAtEnd=true
  let observer=model.objectWillChange.sink { _ in
   guard armed && model.statusMessage == "Deleted 1 item permanently." else {return};armed=false
   model.root=nil;model.statusMessage="new status";model.updatesPaused=false
  }
  let end=model.$isRemovingAcrossSessions.dropFirst().sink { active in
   if !active {statusAtEnd=model.statusMessage;pausedAtEnd=model.updatesPaused}
  }
  gate.release();try await wait {!model.isRemovingAcrossSessions};observer.cancel();end.cancel()
  guard !armed,statusAtEnd=="new status",!pausedAtEnd else {throw NSError(domain:"monitor invalidation overwrote feedback",code:1)}
  print("PASS: monitor failure respects root invalidation and newer pause state")
 }
 @MainActor static func newerFeedback(_ base:URL) async throws {
  let (model,_,_)=try await create(base),gate=Gate()
  defer {gate.release();model.cancelScan()}
  model.testBeforeRemovalMerge={gate.pauseOnce()};model.deleteCollectionPermanently();try await wait {gate.waiting}
  var armed=true,errorAtEnd:String?,statusAtEnd:String?,pausedAtEnd=false
  let observer=model.objectWillChange.sink { _ in
   guard armed else{return};armed=false;model.errorMessage="new error";model.statusMessage="new status";model.updatesPaused=true
  }
  let end=model.$isRemovingAcrossSessions.dropFirst().sink { active in
   if !active {errorAtEnd=model.errorMessage;statusAtEnd=model.statusMessage;pausedAtEnd=model.updatesPaused}
  }
  gate.release();try await wait {!model.isRemovingAcrossSessions};observer.cancel();end.cancel()
  guard !armed,errorAtEnd=="new error",statusAtEnd=="new status",pausedAtEnd else {throw NSError(domain:"newer completion feedback lost",code:1)}
  print("PASS: newer feedback and pause state during completion commit are preserved")
 }
 @MainActor static func nested(_ base:URL) async throws {
  let (model,a,b)=try await create(base),first=Gate(),second=Gate()
  defer {first.release();second.release();model.cancelScan()}
  guard let other=model.current?.children.first(where: { node in !model.collection.contains(where: { $0.id == node.id }) }) else {
   throw NSError(domain:"missing second item",code:1)
  }
  model.testBeforeRemoval={first.pauseOnce()};model.deleteCollectionPermanently();try await wait {first.waiting}
  var armed=true, nestedStarted=false, retained=false
  let observer=model.objectWillChange.sink { _ in
   guard armed && !model.isRemoving else {return};armed=false
   model.collection = [other]
   model.testBeforeRemoval={second.pauseOnce()}
   model.deleteCollectionPermanently();nestedStarted=model.isRemoving
   model.testActivateResolved(b);model.testActivateResolved(a)
   retained=model.isRemoving
  }
  first.release();try await wait {second.waiting};observer.cancel()
  guard nestedStarted,retained,model.isRemoving else {throw NSError(domain:"nested removal lost",code:1)}
  second.release();try await wait {!model.isRemovingAcrossSessions}
  guard !model.isRemoving else {throw NSError(domain:"nested removal stuck",code:1)}
  print("PASS: nested removal remains busy across root switch")
 }
 @MainActor static func main() async throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL
  try await replacement(base.appendingPathComponent("replacement"))
  try await invalidate(base.appendingPathComponent("invalidate"))
  try await nested(base.appendingPathComponent("nested"))
  try await feedbackSwitch(base.appendingPathComponent("feedback-switch"))
  try await monitorSwitch(base.appendingPathComponent("monitor-switch"))
  try await monitorInvalidate(base.appendingPathComponent("monitor-invalidate"))
  try await newerFeedback(base.appendingPathComponent("newer-feedback"))
 }
}
