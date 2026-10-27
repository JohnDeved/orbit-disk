@testable import OrbitCore
import Darwin
import Foundation
import CSQLite
@main struct Check {
 static func identity(_ p:String)->String {var s=stat();precondition(lstat(p,&s)==0);return "\(s.st_dev):\(s.st_ino)"}
 static func main() throws {
  let args=CommandLine.arguments, base=URL(fileURLWithPath:args.last!,isDirectory:true).resolvingSymlinksInPath(),root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true),db=base.appendingPathComponent("stage.sqlite").path
  let cp=StageCheckpoint(rootPath:root.path,identity:"test",eventID:0)
  if args.count==3 {
   let c=try DurableCrawler(rootURL:root,database:db,checkpoint:cp,mirror:true),r=try StageRemovalReplay(cache:cache,crawler:c,cancellation:ScanCancellation())
   try r.prepare();guard r.viewState().blocked else {throw NSError(domain:"missing gate",code:1)}
   _ = try c.step();kill(getpid(),SIGKILL);return
  }
  let fm=FileManager.default;try fm.createDirectory(at:root.appendingPathComponent("unrelated",isDirectory:true),withIntermediateDirectories:true)
  let victim=root.appendingPathComponent("victim");try Data([1]).write(to:victim);try Data([2]).write(to:root.appendingPathComponent("unrelated/child"))
  do {
   let c=try DurableCrawler(rootURL:root,database:db,checkpoint:cp,mirror:true),r=try StageRemovalReplay(cache:cache,crawler:c,cancellation:ScanCancellation())
   try r.prepare();guard try r.settleIfPossible() else {throw NSError(domain:"initial",code:1)}
   _=try c.step();while try c.store.hasReplayDirtyWork() {_=try c.step()}
  }
  let journal=try RemovalJournal(cache:cache)
  var items=[ScanRemoval(path:victim.path,identity:identity(victim.path))]
  for i in 0..<10_001 {items.append(ScanRemoval(path:root.appendingPathComponent("missing-\(i)").path,identity:"1:1"))}
  let op=try journal.begin(root:root,rootIdentity:identity(root.path),items:items);try fm.removeItem(at:victim);try op.complete()
  let child=Process();child.executableURL=URL(fileURLWithPath:args[0]);child.arguments=["child",base.path];try child.run();child.waitUntilExit()
  guard child.terminationReason == .uncaughtSignal,child.terminationStatus==SIGKILL else {throw NSError(domain:"child not killed",code:1)}
  let c=try DurableCrawler(rootURL:root,database:db,checkpoint:cp,mirror:true),r=try StageRemovalReplay(cache:cache,crawler:c,cancellation:ScanCancellation())
  try r.prepare();guard r.viewState().blocked else {throw NSError(domain:"restart did not block",code:1)}
  var n=0;while try c.store.hasReplayDirtyWork(){guard n<200 else{throw NSError(domain:"stuck",code:1)};_=try c.step();n+=1}
  guard try r.settleIfPossible(),!r.viewState().blocked,try c.store.hasPendingDirectories() else {throw NSError(domain:"recovery failed",code:1)}
  var h:OpaquePointer?;guard sqlite3_open_v2(db,&h,SQLITE_OPEN_READONLY,nil)==SQLITE_OK else{throw NSError(domain:"open",code:1)};defer{sqlite3_close(h)}
  var q:OpaquePointer?;guard sqlite3_prepare_v2(h,"SELECT count(*) FROM nodes WHERE name='victim'",-1,&q,nil)==SQLITE_OK else{throw NSError(domain:"query",code:1)};defer{sqlite3_finalize(q)}
  guard sqlite3_step(q)==SQLITE_ROW,sqlite3_column_int(q,0)==0 else{throw NSError(domain:"stale row",code:1)}
  print("PASS: SIGKILL during overflow parent repair preserves pending scan and hides stale rows after restart")
 }
}
