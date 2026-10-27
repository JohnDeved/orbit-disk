@testable import OrbitCore
import Darwin
import Foundation
import CSQLite
@main struct Check {
 static func identity(_ path:String) -> String { var v=stat(); precondition(lstat(path,&v)==0); return "\(v.st_dev):\(v.st_ino)" }
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).resolvingSymlinksInPath(),root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true)
  try FileManager.default.createDirectory(at:root,withIntermediateDirectories:true)
  let a=root.appendingPathComponent("a"),b=root.appendingPathComponent("b")
  try Data([1]).write(to:a);try Data([2]).write(to:b)
  let cp=StageCheckpoint(rootPath:root.path,identity:"test",eventID:0)
  let crawler=try DurableCrawler(rootURL:root,database:base.appendingPathComponent("stage.sqlite").path,checkpoint:cp,mirror:true)
  while try crawler.step() {}
  let replay=try StageRemovalReplay(cache:cache,crawler:crawler,cancellation:ScanCancellation())
  try replay.prepare();while try crawler.step() {};guard try replay.settleIfPossible() else {throw NSError(domain:"unstable",code:1)}
  let op=try replay.journal.begin(root:root,rootIdentity:identity(root.path),items:[ScanRemoval(path:a.path,identity:identity(a.path))]);try FileManager.default.removeItem(at:a);try op.complete()
  try replay.prepare();guard replay.viewState().overlay.count==1 && replay.viewState().overlay[0].path==a.path else {throw NSError(domain:"overlay",code:1)}
  while try crawler.step() {};guard try replay.settleIfPossible() else {throw NSError(domain:"unstable",code:1)};guard replay.viewState().overlay.isEmpty else {throw NSError(domain:"overlay retirement",code:1)}
  var db:OpaquePointer?;guard sqlite3_open_v2(crawler.store.databasePath,&db,SQLITE_OPEN_READONLY,nil)==SQLITE_OK else{throw NSError(domain:"open",code:1)};defer{sqlite3_close(db)}
  var stmt:OpaquePointer?;guard sqlite3_prepare_v2(db,"SELECT count(*) FROM nodes WHERE is_dir=0",-1,&stmt,nil)==SQLITE_OK else{throw NSError(domain:"query",code:1)};defer{sqlite3_finalize(stmt)}
  guard sqlite3_step(stmt)==SQLITE_ROW && sqlite3_column_int(stmt,0)==1 else{throw NSError(domain:"stale row",code:1)}
  let op2=try replay.journal.begin(root:root,rootIdentity:identity(root.path),items:[ScanRemoval(path:b.path,identity:identity(b.path))]);try FileManager.default.removeItem(at:b);try op2.complete()
  do {try replay.fencePublication({});throw NSError(domain:"accepted stale watermark",code:1)} catch StageRemovalReplay.Retry.journalChanged {}
  try replay.prepare();while try crawler.step() {};guard try replay.settleIfPossible() else {throw NSError(domain:"unstable",code:1)};try replay.fencePublication({})
  print("PASS: staged overlay persists until reconciliation; stale final watermark rejected and retry succeeds")
 }
}
