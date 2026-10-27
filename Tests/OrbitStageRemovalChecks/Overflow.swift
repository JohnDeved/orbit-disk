@testable import OrbitCore
import Darwin
import Foundation
import CSQLite
@main struct Check {
 static func identity(_ path:String)->String { var s=stat(); precondition(lstat(path,&s)==0); return "\(s.st_dev):\(s.st_ino)" }
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).resolvingSymlinksInPath(),root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true)
  let fm=FileManager.default;try fm.createDirectory(at:root.appendingPathComponent("unrelated",isDirectory:true),withIntermediateDirectories:true)
  let victim=root.appendingPathComponent("victim");try Data([1]).write(to:victim)
  try Data([2]).write(to:root.appendingPathComponent("unrelated/child"))
  let crawler=try DurableCrawler(rootURL:root,database:base.appendingPathComponent("stage.sqlite").path,checkpoint:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0),mirror:true)
  let replay=try StageRemovalReplay(cache:cache,crawler:crawler,cancellation:ScanCancellation())
  try replay.prepare();guard try replay.settleIfPossible() else {throw NSError(domain:"initial replay",code:1)}
  _ = try crawler.step(); while try crawler.store.hasReplayDirtyWork() {_ = try crawler.step()}
  guard try crawler.store.hasPendingDirectories() else {throw NSError(domain:"fixture did not leave unrelated work",code:1)}
  var items=[ScanRemoval(path:victim.path,identity:identity(victim.path))]
  for i in 0..<10_001 { items.append(ScanRemoval(path:root.appendingPathComponent("missing-\(i)").path,identity:"1:1")) }
  let op=try replay.journal.begin(root:root,rootIdentity:identity(root.path),items:items);try fm.removeItem(at:victim);try op.complete()
  try replay.prepare();guard replay.viewState().blocked && replay.viewState().overlay.isEmpty else {throw NSError(domain:"overlay overflow did not block",code:1)}
  var steps=0
  while try crawler.store.hasReplayDirtyWork() { guard steps < 200 else {throw NSError(domain:"dirty work stuck",code:1)};_ = try crawler.step();steps += 1 }
  guard try replay.settleIfPossible(), !replay.viewState().blocked, try crawler.store.hasPendingDirectories() else {throw NSError(domain:"unrelated scan unnecessarily completed or preview blocked",code:1)}
  var db:OpaquePointer?; guard sqlite3_open_v2(crawler.store.databasePath,&db,SQLITE_OPEN_READONLY,nil)==SQLITE_OK else{throw NSError(domain:"db",code:1)};defer{sqlite3_close(db)}
  var stmt:OpaquePointer?;guard sqlite3_prepare_v2(db,"SELECT count(*) FROM nodes WHERE name='victim'",-1,&stmt,nil)==SQLITE_OK else{throw NSError(domain:"query",code:1)};defer{sqlite3_finalize(stmt)}
  guard sqlite3_step(stmt)==SQLITE_ROW,sqlite3_column_int(stmt,0)==0 else{throw NSError(domain:"stale victim",code:1)}
  print("PASS: >10k removals in one parent remain bounded, repair that parent, and restore browsing with unrelated scan pending")
 }
}
