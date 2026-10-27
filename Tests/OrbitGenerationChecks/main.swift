import Darwin
import Foundation
final class Outcome: @unchecked Sendable {
 private let lock=NSLock(); private var ok=false
 func mark() { lock.lock();ok=true;lock.unlock() }
 var success: Bool { lock.lock();defer{lock.unlock()};return ok }
}
@main struct Check {
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL, fm=FileManager.default
  func fixture(_ name:String) throws -> (URL,URL,DiskIndexStore,IndexSnapshot,StageCheckpoint) {
   let root=base.appendingPathComponent(name+"-root",isDirectory:true).standardizedFileURL, cache=base.appendingPathComponent(name+"-cache",isDirectory:true).standardizedFileURL
   try fm.createDirectory(at:root,withIntermediateDirectories:true); try fm.createDirectory(at:cache,withIntermediateDirectories:true)
   try Data([1]).write(to:root.appendingPathComponent("old"))
   let store=DiskIndexStore(directory:cache), saved=try store.save(DiskScanner().scan(root),eventID:3,eventIdentity:"test")
   try Data([2]).write(to:root.appendingPathComponent("new"))
   return (root,cache,store,saved,StageCheckpoint(rootPath:root.path,identity:"test",eventID:4))
  }
  do {
   let (root,cache,store,_,cp)=try fixture("active")
   let stage=try ResumableStage(cache:cache,root:root,current:cp)
   while try stage.advance(progress:{_ in}) {}
   try fm.removeItem(at:cache)
   let saved=try stage.publish(elapsed:0)
   guard try store.loadFolder(path:root.path,snapshot:saved).children.count == 2 else { throw NSError(domain:"active purge",code:1) }
   print("PASS: cache purge during a completed scan republishes retained stage")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("resume")
   do { let stage=try ResumableStage(cache:cache,root:root,current:cp); _ = try stage.advance(progress:{_ in}) }
   try fm.removeItem(at:cache)
   let stage=try ResumableStage(cache:cache,root:root,current:cp)
   while try stage.advance(progress:{_ in}) {}
   let saved=try stage.publish(elapsed:0)
   guard try store.loadFolder(path:root.path,snapshot:saved).children.count == 2 else { throw NSError(domain:"resume purge",code:1) }
   print("PASS: interrupted scan resumes after cache purge")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("invalidated")
   let stage=try ResumableStage(cache:cache,root:root,current:cp)
   while try stage.advance(progress:{_ in}) {}
   try store.invalidate(for:root)
   do { _ = try stage.publish(elapsed:0); fatalError("published invalidated stage") } catch SnapshotExpectation.Failure.changed {}
   guard store.savedSnapshot(for:root) == nil else { throw NSError(domain:"invalidated stage restored",code:1) }
   print("PASS: explicit invalidation blocks an active stale publisher")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("concurrent-invalidation")
   let stage=try ResumableStage(cache:cache,root:root,current:cp)
   while try stage.advance(progress:{_ in}) {}
   let entered=DispatchSemaphore(value:0), release=DispatchSemaphore(value:0), publishDone=DispatchSemaphore(value:0), invalidateDone=DispatchSemaphore(value:0)
   let published=Outcome(), invalidated=Outcome()
   DiskIndexStore.testBeforePublish={ entered.signal(); release.wait() }
   DispatchQueue.global().async { defer {publishDone.signal()}; do { _ = try stage.publish(elapsed:0); published.mark() } catch {} }
   guard entered.wait(timeout:.now()+5) == .success else { throw NSError(domain:"publication gate",code:1) }
   DispatchQueue.global().async { defer {invalidateDone.signal()}; do { try store.invalidate(for:root); invalidated.mark() } catch {} }
   guard invalidateDone.wait(timeout:.now()+0.2) == .timedOut else { release.signal(); throw NSError(domain:"invalidation bypassed root lease",code:1) }
   release.signal()
   guard publishDone.wait(timeout:.now()+5) == .success, invalidateDone.wait(timeout:.now()+5) == .success,
         published.success, invalidated.success, store.savedSnapshot(for:root) == nil else { throw NSError(domain:"concurrent invalidate",code:1) }
   DiskIndexStore.testBeforePublish=nil
   print("PASS: concurrent invalidation waits for publication and removes its result")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("marker-crash")
   let stage=try ResumableStage(cache:cache,root:root,current:cp)
   while try stage.advance(progress:{_ in}) {}
   do { let lease=try RootMutationLease(directory:store.lockDirectory,root:root); try lease.invalidateGeneration() }
   guard store.savedSnapshot(for:root) != nil else { throw NSError(domain:"marker crash removed saved index",code:1) }
   do { _ = try stage.publish(elapsed:0); fatalError("published after invalidation marker") } catch SnapshotExpectation.Failure.changed {}
   print("PASS: crash after durable invalidation marker cannot publish stale stage")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("finished-purge")
   var firstID=""
   do { let stage=try ResumableStage(cache:cache,root:root,current:cp); while try stage.advance(progress:{_ in}) {}; firstID=try stage.publish(elapsed:0).id }
   try fm.removeItem(at:cache)
   let stage=try ResumableStage(cache:cache,root:root,current:cp)
   while try stage.advance(progress:{_ in}) {}
   let restored=try stage.publish(elapsed:0)
   guard restored.id == firstID, try store.loadFolder(path:root.path,snapshot:restored).children.count == 2 else { throw NSError(domain:"finished stage purge",code:1) }
   print("PASS: published stage restores a purged saved index without remapping")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("purge-then-invalidate")
   do { let stage=try ResumableStage(cache:cache,root:root,current:cp); _ = try stage.advance(progress:{_ in}) }
   try fm.removeItem(at:cache); try store.invalidate(for:root)
   do { _ = try ResumableStage(cache:cache,root:root,current:cp); fatalError("resumed explicitly invalidated stage") } catch ResumableStage.Failure.invalidated {}
   try ResumableStage.retireInvalidated(cache:cache,root:root)
   let fresh=try ResumableStage(cache:cache,root:root,current:cp)
   while try fresh.advance(progress:{_ in}) {}
   let rebuilt=try fresh.publish(elapsed:0)
   guard try store.loadFolder(path:root.path,snapshot:rebuilt).children.count == 2 else { throw NSError(domain:"rebuild invalidated map",code:1) }
   print("PASS: purge then invalidation prevents stale resume and allows a fresh mapping")
  }
  do {
   let (root,cache,store,_,cp)=try fixture("interrupted-invalidated")
   do { let stage=try ResumableStage(cache:cache,root:root,current:cp); _ = try stage.advance(progress:{_ in}) }
   try store.invalidate(for:root); try fm.removeItem(at:cache)
   do { _ = try ResumableStage(cache:cache,root:root,current:cp); fatalError("resumed invalidated stage") } catch ResumableStage.Failure.invalidated {}
   guard store.savedSnapshot(for:root) == nil else { throw NSError(domain:"invalidated stage restored after purge",code:1) }
   print("PASS: explicit invalidation survives interruption and cache deletion")
  }
 }
}
