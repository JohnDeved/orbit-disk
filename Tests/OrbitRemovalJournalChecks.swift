import CSQLite
import Darwin
import Foundation
import OrbitCore
@main struct Checks {
 static func main() throws {
  let args=CommandLine.arguments, base=URL(fileURLWithPath:args[2],isDirectory:true), root=base.appendingPathComponent("root"), nested=root.appendingPathComponent("nested"), target=nested.appendingPathComponent("target"), other=nested.appendingPathComponent("other"), cache=base.appendingPathComponent("cache")
  if args[1]=="child" {
   #if ORBIT_TESTING
   if args[3]=="uncommitted" { RemovalJournal.testBeforePendingCommit = { _exit(47) } }
   #endif
   let journal=try RemovalJournal(cache:cache), op=try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:target.path,identity:"old-id"),ScanRemoval(path:other.path,identity:"other-id")])
   try Data().write(to:base.appendingPathComponent("ready"))
   while !FileManager.default.fileExists(atPath:base.appendingPathComponent("go").path) {usleep(10000)}
   if args[3]=="partial" {try FileManager.default.removeItem(at:target)}
   withExtendedLifetime(op) { _exit(47) }
  }
  let fm=FileManager.default;try fm.createDirectory(at:nested,withIntermediateDirectories:true);try fm.createDirectory(at:base.appendingPathComponent("root-other"),withIntermediateDirectories:true);try Data([1]).write(to:target);try Data([2]).write(to:other)
  let journal=try RemovalJournal(cache:cache)
  #if ORBIT_TESTING
  let aborted=Process();aborted.executableURL=URL(fileURLWithPath:args[0]);aborted.arguments=["child",base.path,"uncommitted"];try aborted.run();aborted.waitUntilExit();precondition(aborted.terminationStatus==47)
  let empty=try journal.withFence { try $0.operations(after:0,limit:5) };precondition(empty.isEmpty)
  #endif
  let initialIdentity=try journal.withFence { $0.identity }
  let child=Process();child.executableURL=URL(fileURLWithPath:args[0]);child.arguments=["child",base.path,"partial"];try child.run()
  for _ in 0..<500 where !fm.fileExists(atPath:base.appendingPathComponent("ready").path) {usleep(10000)}
  let live=try journal.withFence { fence -> Bool in
   let entries=try fence.operations(after:0,limit:5);precondition(entries.count==1 && !entries[0].completed && fence.highWater==1 && fence.identity==initialIdentity)
   let paths=try fence.intersections(sequence:entries[0].sequence,root:nested,limit:5);precondition(paths.count==2)
   let resolvedNested=nested.resolvingSymlinksInPath()
   let aliasMatches=try fence.intersections(sequence:entries[0].sequence,root:resolvedNested,limit:5)
   precondition(aliasMatches.count==2 && aliasMatches.contains(where: { $0.removal.path == resolvedNested.appendingPathComponent("target").path && $0.relation == .inside }))
   let physical=URL(fileURLWithPath:"/System/Volumes/Data" + resolvedNested.path,isDirectory:true)
   var actual=stat(), mirrored=stat()
   if stat(resolvedNested.path,&actual)==0 && stat(physical.path,&mirrored)==0 && actual.st_dev==mirrored.st_dev && actual.st_ino==mirrored.st_ino {
    let mirroredMatches=try fence.intersections(sequence:entries[0].sequence,root:physical,limit:5)
    precondition(mirroredMatches.count==2 && mirroredMatches.contains(where: { $0.removal.path == physical.appendingPathComponent("target").path && $0.relation == .inside }))
   }
   let unrelated=try fence.intersections(sequence:entries[0].sequence,root:base.appendingPathComponent("root-other"),limit:5);precondition(unrelated.isEmpty)
   return try fence.isLive(sequence:entries[0].sequence)
  };precondition(live)
  try Data().write(to:base.appendingPathComponent("go"));child.waitUntilExit();precondition(child.terminationStatus==47)
  let abandoned=try journal.withFence { fence in try !fence.isLive(sequence:1) };precondition(abandoned)
  try Data([3]).write(to:target)
  let oldIdentity=try journal.withFence { fence in try fence.intersections(sequence:1,root:root,limit:5).first(where:{$0.originalPath==target.path})?.removal.identity }
  precondition(oldIdentity=="old-id")
  let entries=(0..<50000).map {ScanRemoval(path:root.appendingPathComponent("file-\($0)").path,identity:"1:\($0+1)")}
  let operation=try journal.begin(root:root,rootIdentity:"root-id",items:entries)
  let rows=try journal.withFence { fence -> Int in
   let ops=try fence.operations(after:1,limit:5);precondition(ops.count==1 && fence.highWater==2)
   var after:String?,count=0
   while true {let page=try fence.intersections(sequence:2,root:root,afterPath:after,limit:512);count += page.count;if page.count<512 {break};after=page.last!.originalPath}
   return count
  };precondition(rows==50000)
  try operation.complete();let completed=try journal.withFence { try $0.operations(after:1,limit:5) };precondition(completed[0].completed)
  let folderOp=try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:nested.path,identity:"folder-id")])
  let ancestor=try journal.withFence { try $0.intersections(sequence:folderOp.sequence,root:nested,limit:5) }
  precondition(ancestor.count==1 && ancestor[0].relation == .coversRoot)
  let parentDir=root.appendingPathComponent("parent-changing")
  try fm.createDirectory(at:parentDir,withIntermediateDirectories:true)
  let changingPath=parentDir.appendingPathComponent("child")
  let changingOp=try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:changingPath.path,identity:"child-id")])
  precondition(changingOp.parentIsUnchanged(path:changingPath.path))
  try fm.moveItem(at:parentDir,to:root.appendingPathComponent("old-parent"))
  try fm.createDirectory(at:parentDir,withIntermediateDirectories:true)
  precondition(!changingOp.parentIsUnchanged(path:changingPath.path))
  let missing=root.appendingPathComponent("missing/child")
  let unresolvedOp=try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:missing.path,identity:"id")])
  precondition(!unresolvedOp.parentIsUnchanged(path:missing.path), "an unresolved parent must not authorize removal")
  let unresolved=try journal.withFence { try $0.intersections(sequence:unresolvedOp.sequence,root:base.appendingPathComponent("unrelated"),limit:5) }
  precondition(unresolved.count==1 && unresolved[0].relation == .unresolved)
  let movable=root.appendingPathComponent("movable"), movedParent=root.appendingPathComponent("moved-parent")
  try fm.createDirectory(at:movable,withIntermediateDirectories:true)
  let movedItem=movable.appendingPathComponent("child")
  let movedOp=try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:movedItem.path,identity:"id")])
  precondition(movedOp.parentIsUnchanged(path:movedItem.path))
  var beforeMove=stat(), afterMove=stat()
  precondition(stat(movable.path,&beforeMove)==0)
  try fm.moveItem(at:movable,to:movedParent)
  try fm.createSymbolicLink(at:movable,withDestinationURL:movedParent)
  precondition(stat(movable.path,&afterMove)==0 && beforeMove.st_dev==afterMove.st_dev && beforeMove.st_ino==afterMove.st_ino)
  precondition(!movedOp.parentIsUnchanged(path:movedItem.path), "same-inode parent moved through a symlink must not authorize removal")
  let newDir=root.appendingPathComponent("new-location"), oldDir=root.appendingPathComponent("old-location")
  try fm.createDirectory(at:newDir,withIntermediateDirectories:true)
  try Data([4]).write(to:newDir.appendingPathComponent("file"))
  try fm.createSymbolicLink(at:oldDir,withDestinationURL:newDir)
  let oldFile=oldDir.appendingPathComponent("file")
  let aliasOp=try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:oldFile.path,identity:"moved-id")])
  let moved=try journal.withFence {try $0.intersections(sequence:aliasOp.sequence,root:root,limit:5)}
  precondition(moved.count==1 && moved[0].removal.path==newDir.appendingPathComponent("file").path && moved[0].originalPath==oldFile.path && moved[0].originalRelation == .inside)
  let lexicalOnly=try journal.withFence {try $0.intersections(sequence:aliasOp.sequence,root:oldDir,limit:5)}
  precondition(lexicalOnly.count==1 && lexicalOnly[0].removal.path==oldFile.path)
  withExtendedLifetime((folderOp,changingOp,unresolvedOp,movedOp,aliasOp)) {}
  let pendingSequences = try journal.withFence { fence -> [Int64] in
   var result: [Int64] = [], after: Int64 = 0
   while true {
    let page = try fence.pendingOperations(after:after,limit:1)
    guard let next = page.first else { break }
    precondition(!next.completed && next.sequence > after)
    result.append(next.sequence); after = next.sequence
   }
   return result
  }
  precondition(pendingSequences.contains(1) && !pendingSequences.contains(2) && pendingSequences.contains(aliasOp.sequence))
  let database=base.appendingPathComponent("cache.staging/.orbit-removals/journal.sqlite")
  var rawDB: OpaquePointer?
  precondition(sqlite3_open_v2(database.path,&rawDB,SQLITE_OPEN_READWRITE,nil) == SQLITE_OK)
  precondition(sqlite3_exec(rawDB,"BEGIN IMMEDIATE; DROP INDEX pending_operations; PRAGMA user_version=2; COMMIT;",nil,nil,nil) == SQLITE_OK)
  sqlite3_close(rawDB)
  let migrated = try journal.withFence { fence -> [Int64] in
   precondition(fence.identity == initialIdentity)
   return try fence.pendingOperations(after:0,limit:512).map(\.sequence)
  }
  precondition(migrated == pendingSequences, "migration must preserve the journal and its pending operations")
  let token=ScanCancellation();token.cancel();do {_ = try journal.withFence(cancellation:token) { _ in true };fatalError("cancel ignored")} catch is CancellationError {}
  do {_ = try journal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:base.appendingPathComponent("escape").path,identity:"id")]);fatalError("outside root")} catch RemovalJournal.Failure.invalid {}
  let held=DispatchSemaphore(value:0), release=DispatchSemaphore(value:0), finished=DispatchSemaphore(value:0)
  DispatchQueue.global().async {
   do { try journal.withFence { _ in held.signal(); release.wait() } } catch { fatalError("fence holder: \(error)") }
   finished.signal()
  }
  precondition(held.wait(timeout:.now()+3) == .success)
  let waitingToken=ScanCancellation(), cancelled=DispatchSemaphore(value:0)
  DispatchQueue.global().async {
   do { try journal.withFence(cancellation:waitingToken) { _ in }; fatalError("waiting fence ignored cancellation") }
   catch is CancellationError {cancelled.signal()} catch {fatalError("wrong cancellation error: \(error)")}
  }
  usleep(30000);waitingToken.cancel();precondition(cancelled.wait(timeout:.now()+3) == .success)
  release.signal();precondition(finished.wait(timeout:.now()+3) == .success)
  let unsafeCache=base.appendingPathComponent("unsafe-cache"), unsafeJournal=try RemovalJournal(cache:unsafeCache)
  let unsafeDir=base.appendingPathComponent("unsafe-cache.staging/.orbit-removals")
  try fm.createSymbolicLink(at:unsafeDir.appendingPathComponent("journal.lock"),withDestinationURL:target)
  do {try unsafeJournal.withFence { _ in };fatalError("symlink lock accepted")} catch RemovalJournal.Failure.unsafe {}
  let brokenCache=base.appendingPathComponent("broken-cache"), brokenJournal=try RemovalJournal(cache:brokenCache)
  let brokenOp=try brokenJournal.begin(root:root,rootIdentity:"root-id",items:[ScanRemoval(path:target.path,identity:"old-id")])
  let brokenDir=base.appendingPathComponent("broken-cache.staging/.orbit-removals")
  let opFile=try fm.contentsOfDirectory(at:brokenDir,includingPropertiesForKeys:nil).first(where:{$0.lastPathComponent.hasSuffix(".operation")})!
  try fm.removeItem(at:opFile)
  do {_ = try brokenJournal.withFence { try $0.isLive(sequence:1) };fatalError("missing operation lock accepted")} catch RemovalJournal.Failure.unsafe {}
  precondition(!fm.fileExists(atPath:opFile.path), "liveness probing must not recreate a lost lock")
  withExtendedLifetime(brokenOp) {}
  let recreatedCache=base.appendingPathComponent("recreated-cache"), recreated=try RemovalJournal(cache:recreatedCache)
  let firstID=try recreated.withFence { $0.identity }
  let db=base.appendingPathComponent("recreated-cache.staging/.orbit-removals/journal.sqlite")
  try fm.removeItem(at:db)
  let secondID=try recreated.withFence { $0.identity };precondition(firstID != secondID)
  let historyBase=base.appendingPathComponent("history"), historyRoot=historyBase.appendingPathComponent("root")
  let historyCache=historyBase.appendingPathComponent("cache"), historyVictim=historyRoot.appendingPathComponent("victim")
  try fm.createDirectory(at:historyRoot,withIntermediateDirectories:true);try Data([1]).write(to:historyVictim)
  let history=try RemovalJournal(cache:historyCache)
  _ = try history.withFence { $0.highWater }
  let historyDB=historyBase.appendingPathComponent("cache.staging/.orbit-removals/journal.sqlite")
  var historyRaw: OpaquePointer?
  precondition(sqlite3_open_v2(historyDB.path,&historyRaw,SQLITE_OPEN_READWRITE,nil)==SQLITE_OK)
  let historySQL="""
  BEGIN IMMEDIATE;
  WITH RECURSIVE numbers(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM numbers WHERE n<260)
  INSERT INTO operations(seq,root,root_identity,lock_name,complete)
  SELECT n,CAST('/unrelated' AS BLOB),'1:1',printf('%08x-0000-0000-0000-000000000000.operation',n),1 FROM numbers;
  INSERT INTO items(op,path,identity,canonical,parent_identity)
  SELECT seq,CAST('/unrelated/file' AS BLOB),'1:2',CAST('/unrelated/file' AS BLOB),'1:1' FROM operations;
  COMMIT;
  """
  precondition(sqlite3_exec(historyRaw,historySQL,nil,nil,nil)==SQLITE_OK);sqlite3_close(historyRaw)
  let liveHistory=try history.begin(root:historyRoot,rootIdentity:"1:1",items:[ScanRemoval(path:historyVictim.path,identity:"1:2")])
  let liveRecovery=try history.recover(root:historyRoot,cursor:nil,cancellation:ScanCancellation())
  precondition(liveRecovery.hasLiveOperation && liveRecovery.removals.contains(where:{$0.path == historyVictim.path}))
  try liveHistory.complete()
  let completedRecovery=try history.recover(root:historyRoot,cursor:nil,cancellation:ScanCancellation())
  precondition(!completedRecovery.hasLiveOperation && completedRecovery.removals.contains(where:{$0.path == historyVictim.path}))
  #if ORBIT_TESTING
  RemovalJournal.testAfterRecoveryProbe = { fatalError("idle recovery probed an operation") }
  #endif
  let idle=try history.recover(root:historyRoot,cursor:completedRecovery.cursor,cancellation:ScanCancellation())
  precondition(idle.removals.isEmpty && !idle.hasLiveOperation)
  #if ORBIT_TESTING
  let movingA=historyBase.appendingPathComponent("A"),movingB=historyBase.appendingPathComponent("B")
  let movingRoot=movingA.appendingPathComponent("root")
  try fm.createDirectory(at:movingRoot,withIntermediateDirectories:true)
  RemovalJournal.testAfterRecoveryProbe = {
   if !FileManager.default.fileExists(atPath:movingB.path) {
    try! FileManager.default.moveItem(at:movingA,to:movingB)
    try! FileManager.default.createSymbolicLink(at:movingA,withDestinationURL:movingB)
   }
  }
  do { _ = try history.recover(root:movingRoot,cursor:nil,cancellation:ScanCancellation());fatalError("changed root alias accepted") }
  catch RemovalRecoveryError.journalChanged {}
  RemovalJournal.testAfterRecoveryProbe = nil
  #endif
  print("PASS: rollback before commit, crash/liveness, partial deletion, replacement identity, overlapping aliases, missing/replaced parents, bounded 50k-item pages, cancellation and unsafe-lock rejection")
 }
}
