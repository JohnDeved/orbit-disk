import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(), semaphore=DispatchSemaphore(value:0); private var flag=false
 var waiting:Bool {lock.lock(); defer{lock.unlock()}; return flag}
 func pause(){lock.lock(); flag=true; lock.unlock(); semaphore.wait(); lock.lock(); flag=false; lock.unlock()}
 func release(){semaphore.signal()}
}
final class AttachProof: @unchecked Sendable {
 let lock=NSLock(); private var count=0
 func record(_ stage:ResumableStage,root:URL){
  let found=(0..<1200).contains { i in (try? stage.testUnprojectedNode(path:root.appendingPathComponent("item-\(i)").path)) != nil }
  lock.lock();if found {count=1};lock.unlock()
 }
 var recorded:Int {lock.lock();defer{lock.unlock()};return count}
}
enum Injected: Error {case failure}
@main struct Check {
 @MainActor static func wait(_ label:String,_ condition:()->Bool) async throws {
  for _ in 0..<500 { if condition(){return};try await Task.sleep(nanoseconds:20_000_000) }
  throw NSError(domain:label,code:1)
 }
 @MainActor static func main() async throws {
  setbuf(stdout,nil)
  let dir=URL(fileURLWithPath:CommandLine.arguments[1]),fm=FileManager.default
  for mode in ["offscreen", "onscreen", "replacement", "saved-refresh", "reentrant", "inflight"] {
   print("START: \(mode)")
   let a=dir.appendingPathComponent(mode+"-a"),b=dir.appendingPathComponent(mode+"-b"),child=a.appendingPathComponent("child"),cache=dir.appendingPathComponent(mode+"-cache")
   try fm.createDirectory(at:child,withIntermediateDirectories:true);try fm.createDirectory(at:b,withIntermediateDirectories:true)
   for i in 0..<1200 {try Data([1]).write(to:a.appendingPathComponent("item-\(i)"))}
   try Data([2]).write(to:child.appendingPathComponent("nested"));try Data([2]).write(to:b.appendingPathComponent("other"))
   let gate=Gate(), proof=AttachProof(), model=DiskModel(cacheDirectory:cache,preferences:UserDefaults(suiteName:UUID().uuidString)!)
   if mode == "reentrant" {
    model.scan(b);try await wait("reentrant B baseline") {!model.isScanning && !model.isOpeningFolder && model.root?.id == b.path}
   }
   if mode == "saved-refresh" {
    model.scan(a);try await wait("saved baseline") {!model.isScanning && !model.isOpeningFolder && model.root?.fileCount == 1201}
   }
   model.testScanner=DiskScanner(testBatch:{gate.pause();throw Injected.failure},testFallback:{throw Injected.failure})
   if mode == "saved-refresh" {model.rescan()} else {model.scan(a)}
   try await wait("A partial before error") {gate.waiting && (model.isPartialMap || model.isRefreshing)}
   model.testScanner=DiskScanner(); model.testConfigureStage={proof.record($0,root:a)}
   if mode == "offscreen" {
    model.scan(b);try await wait("B opened") {model.root?.id == b.path && !model.isScanning && !model.isOpeningFolder}
    gate.release();try await wait("A failed after release") {!gate.waiting}
    try await Task.sleep(nanoseconds:300_000_000);model.scan(child)
   } else if mode == "onscreen" || mode == "saved-refresh" {
    gate.release();try await wait("visible failure") {model.errorMessage != nil && !model.isScanning}
    model.scan(child)
   } else if mode == "inflight" {
    let resolutionGate=Gate();model.testBeforeResolution={resolutionGate.pause()}
    model.scan(b);try await wait("B resolution gated") {resolutionGate.waiting}
    model.isOpeningFolder=true
    gate.release();try await wait("A failure while B resolving") {model.testScanFailed(a.path) && !model.isScanning}
    precondition(model.isOpeningFolder,"failure must not clear the newer opening request")
    resolutionGate.release();model.testBeforeResolution={}
    try await wait("B resolution completes") {model.root?.id == b.path && !model.isOpeningFolder && !model.isScanning}
    model.scan(child)
   } else if mode == "reentrant" {
    try await wait("A visible before reentrant failure") { model.root?.id == a.path }
    var fired=false
    let observer=model.objectWillChange.sink {
     MainActor.assumeIsolated {
      if !fired && model.testScanFailed(a.path) && model.root?.id == a.path {
       fired=true;model.scan(b)
      }
     }
    }
    gate.release();try await wait("reentrant switch") {fired && model.root?.id == b.path && !model.isOpeningFolder && !model.isScanning}
    precondition(model.errorMessage == nil && !model.updatesPaused, "old scan feedback must not be applied to the new root")
    withExtendedLifetime(observer) {}
    model.scan(child)
   } else {
    model.rescan();gate.release()
   }
   try await wait("A resumed \(mode)") {model.root?.id == a.path && !model.isScanning && !model.isRefreshing && !model.isOpeningFolder && !model.isPartialMap && model.root?.fileCount == 1201 && proof.recorded == 1}
   if mode != "replacement" {precondition(model.current?.id == child.path,"nested navigation must resume its original root")}
   precondition(proof.recorded == 1,"resume should retain a committed stage row")
   print("PASS: \(mode) reopens its committed stage and finishes")
   model.cancelScan()
  }
 }
}
