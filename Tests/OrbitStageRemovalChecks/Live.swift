@testable import OrbitCore
import Darwin
import Foundation
@main struct Check {
 static func id(_ p:String)->String {var s=stat();precondition(lstat(p,&s)==0);return "\(s.st_dev):\(s.st_ino)"}
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).resolvingSymlinksInPath(),root=base.appendingPathComponent("root",isDirectory:true),cache=base.appendingPathComponent("cache",isDirectory:true)
  let fm=FileManager.default;try fm.createDirectory(at:root.appendingPathComponent("unrelated",isDirectory:true),withIntermediateDirectories:true)
  let victim=root.appendingPathComponent("victim");try Data([1]).write(to:victim);try Data([1]).write(to:root.appendingPathComponent("unrelated/child"))
  let c=try DurableCrawler(rootURL:root,database:base.appendingPathComponent("stage.sqlite").path,checkpoint:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0),mirror:true)
  let r=try StageRemovalReplay(cache:cache,crawler:c,cancellation:ScanCancellation())
  try r.prepare();guard try r.settleIfPossible() else{throw NSError(domain:"initial",code:1)}
  _=try c.step();while try c.store.hasReplayDirtyWork(){_=try c.step()}
  let operation=try r.journal.begin(root:root,rootIdentity:id(root.path),items:[ScanRemoval(path:victim.path,identity:id(victim.path))])
  let start=ProcessInfo.processInfo.systemUptime;try r.prepare()
  guard ProcessInfo.processInfo.systemUptime-start < 0.5,!r.viewState().blocked,r.viewState().overlay.count==1 else{throw NSError(domain:"live prepare blocked",code:1)}
  guard try !r.settleIfPossible(),try c.step() else{throw NSError(domain:"live barrier blocked unrelated work",code:1)}
  try fm.removeItem(at:victim);try operation.complete()
  usleep(1_100_000);try r.refreshIfNeeded()
  var n=0;while try c.store.hasReplayDirtyWork(){guard n<100 else{throw NSError(domain:"dirty stuck",code:1)};_=try c.step();n+=1}
  guard try r.settleIfPossible(),r.viewState().overlay.isEmpty,r.viewState().cursor?.sequence==1 else{throw NSError(domain:"did not settle after live completion",code:1)}
  let appRoot=base.appendingPathComponent("app-root",isDirectory:true),appCache=base.appendingPathComponent("app-cache",isDirectory:true)
  try fm.createDirectory(at:appRoot,withIntermediateDirectories:true)
  let waiting=appRoot.appendingPathComponent("waiting");try Data([1]).write(to:waiting)
  let stage=try ResumableStage(cache:appCache,root:appRoot,current:StageCheckpoint(rootPath:appRoot.path,identity:"test",eventID:0))
  try stage.prepareRemovalReplay();while try stage.advance(progress:{_ in}){}
  let appJournal=try RemovalJournal(cache:appCache)
  let active=try appJournal.begin(root:appRoot,rootIdentity:id(appRoot.path),items:[ScanRemoval(path:waiting.path,identity:id(waiting.path))])
  try stage.prepareRemovalReplay();let revision=stage.revision
  _=try stage.advance(progress:{_ in});_=try stage.advance(progress:{_ in})
  guard stage.revision==revision else {throw NSError(domain:"idle live barrier changed preview revision",code:1)}
  try fm.removeItem(at:waiting);try active.complete()
  while try stage.advance(progress:{_ in}){}
  print("PASS: live deletion overlays without blocking unrelated mapping; completion requeues the parent before cursor acknowledgement")
 }
}
