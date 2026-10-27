@testable import OrbitCore
import Darwin
import Foundation
final class Flag: @unchecked Sendable { let lock=NSLock();var error:Error?;func capture(_ body:() throws -> Void){do{try body()}catch{lock.lock();self.error=error;lock.unlock()}} }
@main struct Check {
 static func identity(_ p:String)->String {var s=stat();precondition(lstat(p,&s)==0);return "\(s.st_dev):\(s.st_ino)"}
 static func make(_ base:URL,_ label:String) throws -> (URL,URL) {let r=base.appendingPathComponent(label,isDirectory:true),c=base.appendingPathComponent(label+"-cache",isDirectory:true);try FileManager.default.createDirectory(at:r,withIntermediateDirectories:true);return(r,c)}
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).resolvingSymlinksInPath(),fm=FileManager.default
  do {
   let (root,cache)=try make(base,"live"),a=root.appendingPathComponent("a"),b=root.appendingPathComponent("b")
   try Data([1]).write(to:a);try Data([1]).write(to:b);let journal=try RemovalJournal(cache:cache)
   let old=try journal.begin(root:root,rootIdentity:identity(root.path),items:[ScanRemoval(path:a.path,identity:identity(a.path))]);try fm.removeItem(at:a);try old.complete()
   let live=try journal.begin(root:root,rootIdentity:identity(root.path),items:[ScanRemoval(path:b.path,identity:identity(b.path))])
   let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0))
   guard stage.removalReplayReadiness().cursor?.sequence==1 else {throw NSError(domain:"skipped live intent",code:1)}
   try stage.prepareRemovalReplay();guard try stage.view(path:nil,removals:[],cancellation:ScanCancellation()) != nil else{throw NSError(domain:"blocked live",code:1)}
   try fm.removeItem(at:b);try live.complete();usleep(1_100_000)
   while try stage.advance(progress:{_ in}) {}
   guard stage.removalReplayReadiness().cursor?.sequence==2 else{throw NSError(domain:"live cursor",code:1)}
  }
  do {
   let (root,cache)=try make(base,"race"),item=root.appendingPathComponent("item");try Data([1]).write(to:item)
   let journal=try RemovalJournal(cache:cache),itemID=identity(item.path),rootID=identity(root.path),flag=Flag()
   StageRemovalReplay.testAfterFreshCapture = {flag.capture {let op=try journal.begin(root:root,rootIdentity:rootID,items:[ScanRemoval(path:item.path,identity:itemID)]);try FileManager.default.removeItem(at:item);try op.complete()}}
   defer {StageRemovalReplay.testAfterFreshCapture=nil}
   let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0))
   if let error=flag.error {throw error}
   guard stage.removalReplayReadiness().cursor?.sequence==0 else {throw NSError(domain:"race cursor",code:1)}
   try stage.prepareRemovalReplay();while try stage.advance(progress:{_ in}) {}
   let saved=try stage.publish(elapsed:0),view=try DiskIndexStore(directory:cache).loadFolder(path:root.path,snapshot:saved)
   guard view.children.isEmpty,stage.removalReplayReadiness().cursor?.sequence==1 else{throw NSError(domain:"race stale",code:1)}
  }
  do {
   let (root,cache)=try make(base,"recreated"),item=root.appendingPathComponent("item");try Data([1]).write(to:item)
   do {let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0));try stage.prepareRemovalReplay();while try stage.advance(progress:{_ in}) {}}
   try fm.removeItem(at:item)
   let journalDir=IndexLocations.locks(for:cache).appendingPathComponent(".orbit-removals",isDirectory:true)
   try fm.removeItem(at:journalDir)
   let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0))
   try stage.prepareRemovalReplay();guard stage.removalReplayReadiness().blocked else{throw NSError(domain:"journal replacement not blocked",code:1)}
   while try stage.advance(progress:{_ in}) {}
   let saved=try stage.publish(elapsed:0),view=try DiskIndexStore(directory:cache).loadFolder(path:root.path,snapshot:saved)
   guard view.children.isEmpty else{throw NSError(domain:"journal replacement stale",code:1)}
  }
  print("PASS: fresh cursor preserves live intent and scan-start races; recreated journal forces full reconciliation")
 }
}
