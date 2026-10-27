@testable import OrbitCore
import Darwin
import Foundation
final class Once: @unchecked Sendable { let lock=NSLock(); var done=false; func claim()->Bool { lock.lock(); defer{lock.unlock()}; if done{return false}; done=true;return true } }
@main struct Check {
 static func identity(_ path:String)->String {var s=stat();precondition(lstat(path,&s)==0);return "\(s.st_dev):\(s.st_ino)"}
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).resolvingSymlinksInPath()
  let root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true)
  try FileManager.default.createDirectory(at:root,withIntermediateDirectories:true)
  let item=root.appendingPathComponent("item");try Data([1]).write(to:item)
  let cp=StageCheckpoint(rootPath:root.path,identity:"test",eventID:0)
  let stage=try ResumableStage(cache:cache,root:root,current:cp)
  try stage.prepareRemovalReplay();while try stage.advance(progress:{_ in}) {}
  let journal=try RemovalJournal(cache:cache), nodeIdentity=identity(item.path), rootIdentity=identity(root.path), once=Once()
  DiskIndexStore.testBeforePublish = {
   guard once.claim() else{return}
   do { let op=try journal.begin(root:root,rootIdentity:rootIdentity,items:[ScanRemoval(path:item.path,identity:nodeIdentity)])
        try FileManager.default.removeItem(at:item);try op.complete() }
   catch {fatalError("injected removal failed: \(error)")}
  }
  defer {DiskIndexStore.testBeforePublish=nil}
  let snapshot=try stage.publish(elapsed:0)
  let folder=try DiskIndexStore(directory:cache).loadFolder(path:root.path,snapshot:snapshot)
  guard once.done && folder.children.isEmpty else {throw NSError(domain:"stale published row",code:1)}
  let cursor=try DiskIndexStore(directory:cache).removalCursor(snapshot:snapshot)
  guard cursor?.sequence == 1 else {throw NSError(domain:"missing removal cursor",code:1)}
  print("PASS: deletion after clone is replayed before the fenced rename and its cursor travels with the snapshot")
 }
}
