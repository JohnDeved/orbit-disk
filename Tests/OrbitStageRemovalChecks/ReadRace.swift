@testable import OrbitCore
import Darwin
import Foundation
final class Gate: @unchecked Sendable {let ready=DispatchSemaphore(value:0),release=DispatchSemaphore(value:0),done=DispatchSemaphore(value:0);let lock=NSLock();var cancelled=false;var error:Error?;func finish(_ e:Error?){lock.lock();defer{lock.unlock()};if e is CancellationError{cancelled=true}else{error=e};done.signal()}}
@main struct Check {
 static func id(_ p:String)->String {var s=stat();precondition(lstat(p,&s)==0);return "\(s.st_dev):\(s.st_ino)"}
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).resolvingSymlinksInPath(),root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true),fm=FileManager.default
  try fm.createDirectory(at:root,withIntermediateDirectories:true)
  for name in ["a","b"] {try Data([1]).write(to:root.appendingPathComponent(name))}
  let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0))
  try stage.prepareRemovalReplay();while try stage.advance(progress:{_ in}){}
  let journal=try RemovalJournal(cache:cache)
  for (name,search) in [("a",false),("b",true)] {
   let item=root.appendingPathComponent(name),gate=Gate()
   ResumableStage.testBeforeStagedRead={gate.ready.signal();gate.release.wait()}
   DispatchQueue.global().async {do {if search {_ = try stage.search(query:name,cancellation:ScanCancellation(),removals:[],limit:10)} else {_ = try stage.view(path:nil,removals:[],cancellation:ScanCancellation())};gate.finish(nil)}catch{gate.finish(error)}}
   guard gate.ready.wait(timeout:.now()+3) == .success else{throw NSError(domain:"read never gated",code:1)}
   let op=try journal.begin(root:root,rootIdentity:id(root.path),items:[ScanRemoval(path:item.path,identity:id(item.path))]);try fm.removeItem(at:item);try op.complete()
   try stage.prepareRemovalReplay();gate.release.signal()
   guard gate.done.wait(timeout:.now()+3) == .success,gate.cancelled else{throw NSError(domain:"stale read returned \(String(describing:gate.error))",code:1)}
   ResumableStage.testBeforeStagedRead=nil
   while try stage.advance(progress:{_ in}){}
  }
  print("PASS: staged view and search spanning replay transitions reject stale results")
 }
}
