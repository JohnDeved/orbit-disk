@testable import OrbitCore
import Darwin
import Foundation
@main struct Check {
 static func id(_ p:String)->String {var s=stat();precondition(lstat(p,&s)==0);return "\(s.st_dev):\(s.st_ino)"}
 static func main() throws {
  let args=CommandLine.arguments,base=URL(fileURLWithPath:args.last!,isDirectory:true).resolvingSymlinksInPath(),root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true)
  if args.count==3 {StageRemovalReplay.testAfterFreshCapture={kill(getpid(),SIGKILL)};_ = try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0));throw NSError(domain:"capture hook missed",code:1)}
  let fm=FileManager.default;try fm.createDirectory(at:root,withIntermediateDirectories:true)
  let a=root.appendingPathComponent("a"),b=root.appendingPathComponent("b");try Data([1]).write(to:a);try Data([1]).write(to:b)
  let journal=try RemovalJournal(cache:cache),rootID=id(root.path)
  let first=try journal.begin(root:root,rootIdentity:rootID,items:[ScanRemoval(path:a.path,identity:id(a.path))]);try fm.removeItem(at:a);try first.complete()
  let child=Process();child.executableURL=URL(fileURLWithPath:args[0]);child.arguments=["child",base.path];try child.run();child.waitUntilExit()
  guard child.terminationReason == .uncaughtSignal,child.terminationStatus==SIGKILL else{throw NSError(domain:"child was not killed",code:1)}
  let second=try journal.begin(root:root,rootIdentity:rootID,items:[ScanRemoval(path:b.path,identity:id(b.path))]);try fm.removeItem(at:b);try second.complete()
  let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0))
  guard stage.hasPriorCheckpoint,stage.removalReplayReadiness().cursor == nil else{throw NSError(domain:"unsafe anchor after crash",code:1)}
  try stage.prepareRemovalReplay();while try stage.advance(progress:{_ in}){}
  let snapshot=try stage.publish(elapsed:0),folder=try DiskIndexStore(directory:cache).loadFolder(path:root.path,snapshot:snapshot)
  guard folder.children.isEmpty,stage.removalReplayReadiness().cursor?.sequence==2 else{throw NSError(domain:"lost intent",code:1)}
  print("PASS: SIGKILL before fresh cursor commit falls back to complete replay")
 }
}
