import CSQLite
import Darwin
import Foundation
struct SimulatedCrash: Error {}
func checkRejectedStageFormats(_ base: URL) throws {
  try FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
  for version in [0, 3] {
    let url = base.appendingPathComponent("format-\(version).sqlite")
    var db: OpaquePointer?
    precondition(sqlite3_open_v2(url.path, &db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK)
    precondition(sqlite3_exec(db, "CREATE TABLE nodes(path TEXT); INSERT INTO nodes VALUES('/kept'); PRAGMA user_version=\(version)", nil, nil, nil) == SQLITE_OK)
    sqlite3_close(db)
    let original = try Data(contentsOf: url)
    do { _ = try ListingStage(path: url.path); fatalError("old inventory format accepted") }
    catch ListingStage.Failure.sqlite {}
    let retained = try Data(contentsOf: url)
    precondition(retained == original, "rejected inventory must remain unchanged")
  }
  let fresh = try ListingStage(path: base.appendingPathComponent("fresh.sqlite").path)
  fresh.close()
  print("PASS: old and unversioned inventories are rejected intact; empty databases initialize")
}
func checkDirectoryRegistrationRollback(_ base: URL) throws {
  let root = base.appendingPathComponent("root", isDirectory: true)
  try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
  let crawler = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path,
                                   checkpoint: StageCheckpoint(rootPath: root.path, identity: "test", eventID: 0))
  let parent = Data(root.path.utf8), device = crawler.root.identity.device
  let epoch = try crawler.store.begin(parent: parent)
  func dir(_ name: String, _ inode: UInt64) -> FileMetadata {
    FileMetadata(name: name, device: device, inode: inode, isDirectory: true, isSymbolicLink: false,
                 isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)
  }
  try crawler.store.append(parent: parent, epoch: epoch, records: [dir("existing", UInt64.max)], rootDevice: device)
  try crawler.store.append(parent: parent, epoch: epoch, records: [dir("existing", UInt64.max)], rootDevice: device)
  do {
    try crawler.store.append(parent: parent, epoch: epoch,
                             records: [dir("new", 1), dir("new", 2)], rootDevice: device)
    fatalError("different identities for a single directory must fail")
  } catch ListingStage.Failure.sqlite { }
  let path = Data((root.path + "/new").utf8)
  guard try crawler.store.directory(path: path) == nil else { fatalError("failed batch leaked directory") }
  let existing = try crawler.store.directory(path: Data((root.path + "/existing").utf8))
  guard existing?.inode == UInt64.max else { fatalError("failed batch changed prior directory") }
  try crawler.store.append(parent: parent, epoch: epoch, records: [dir("new", 3)], rootDevice: device)
  guard try crawler.store.directory(path: path)?.inode == 3 else { fatalError("batch did not recover after rollback") }
  print("PASS: repeated directory registration and mismatched identity rollback")
}
final class Outcome: @unchecked Sendable { private let lock=NSLock(); private var value=false; func set(){lock.lock();value=true;lock.unlock()}; var cancelled:Bool {lock.lock();defer{lock.unlock()};return value} }
func checkIdentityLookup(_ base: URL) throws {
  let root = base.appendingPathComponent("root", isDirectory: true)
  try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
  let crawler = try DurableCrawler(rootURL: root, database: base.appendingPathComponent("stage.sqlite").path,
                                   checkpoint: StageCheckpoint(rootPath: root.path, identity: "test", eventID: 0))
  let parent = Data(root.path.utf8), epoch = try crawler.store.begin(parent: parent)
  let device: Int32 = -3, inode: UInt64 = .max
  func file(_ name: String, _ allocation: Int64) -> FileMetadata {
    FileMetadata(name: name, device: device, inode: inode, isDirectory: false, isSymbolicLink: false,
                 isMountPoint: false, linkCount: 2, size: 9, allocatedSize: allocation)
  }
  let directory = FileMetadata(name: "000-directory", device: device, inode: inode, isDirectory: true,
                               isSymbolicLink: false, isMountPoint: false, linkCount: 2, size: 0, allocatedSize: 0)
  try crawler.store.append(parent: parent, epoch: epoch, records: [directory, file("Case", 512), file("case", 512)])
  while try crawler.store.reconcileLinksChunk() {}
  try crawler.store.append(parent: parent, epoch: epoch, records: [file("Case", 1024), file("case", 1024)])
  while try crawler.store.reconcileLinksChunk() {}
  func charge(_ name: String) throws -> Int64 { try crawler.store.charge(path: Data((root.path + "/" + name).utf8)) ?? -1 }
  let upper = try charge("Case"), lower = try charge("case"), folder = try charge("000-directory")
  precondition(upper + lower == 1024 && folder == 0, "rebuild charged the wrong identity")
  let upperFirst = HardLinkPathOrder.precedes(Data((root.path + "/Case").utf8), Data((root.path + "/case").utf8))
  precondition((upperFirst ? upper : lower) == 1024, "case-distinct owner differs")
  print("PASS: signed device, high-bit inode, case-distinct links and directory identity collision")
}
func checkLookupPreflight(_ base: URL) throws {
  let root = base.appendingPathComponent("root", isDirectory: true)
  try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
  let checkpoint = StageCheckpoint(rootPath: root.path, identity: "test", eventID: 0)
  let database = base.appendingPathComponent("stage.sqlite").path
  let crawler = try DurableCrawler(rootURL: root, database: database, checkpoint: checkpoint)
  let parent = Data(root.path.utf8), device = crawler.root.identity.device
  let epoch = try crawler.store.begin(parent: parent)
  let mount = FileMetadata(name: "mount", device: device, inode: 11, isDirectory: true, isSymbolicLink: false, isMountPoint: true, linkCount: 1, size: 0, allocatedSize: 0)
  let replacement = FileMetadata(name: "mount", device: device, inode: 12, isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 3, allocatedSize: 512)
  try crawler.store.append(parent: parent, epoch: epoch, records: [mount, replacement], rootDevice: device)
  var db: OpaquePointer?
  guard sqlite3_open_v2(database, &db, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { fatalError("open") }
  defer { sqlite3_close(db) }
  func scalar(_ sql: String) -> Int64 {
   var stmt: OpaquePointer?
   guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { fatalError("prepare") }
   defer { sqlite3_finalize(stmt) }
   guard sqlite3_step(stmt) == SQLITE_ROW else { fatalError("read") }
   return sqlite3_column_int64(stmt, 0)
  }
  precondition(scalar("SELECT count(*) FROM stage_issues") == 0, "replacement must clear mount issue in the same batch")
  precondition(scalar("SELECT count(*) FROM nodes AS child JOIN nodes AS parent ON child.parent=parent.id WHERE child.name='mount' AND parent.is_dir=1") == 1, "insert must retain its parent ID")
  let directory = FileMetadata(name: "child", device: device, inode: 21, isDirectory: true, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 0, allocatedSize: 0)
  let file = FileMetadata(name: "child", device: device, inode: 22, isDirectory: false, isSymbolicLink: false, isMountPoint: false, linkCount: 1, size: 1, allocatedSize: 512)
  try crawler.store.append(parent: parent, epoch: epoch, records: [directory], rootDevice: device)
  do { try crawler.store.append(parent: parent, epoch: epoch, records: [file], rootDevice: device); fatalError("directory replacement skipped prune") }
  catch ListingStage.Reconcile.pruneRequired {}
  let pruned = try crawler.store.pruneOneChunk()
  precondition(pruned, "replacement must enter the prune queue")
  print("PASS: mount issue clearing, parent link and directory replacement preflight")
}
@main struct Check {
 static func main() throws {
  let args = CommandLine.arguments
  if args.count == 4 && args[1] == "no-stale-trim-child" {
   try NoStaleTrimChecks.crashChild(URL(fileURLWithPath: args[2], isDirectory: true), boundary: args[3])
   return
  }
  if args.count == 4 && args[1] == "fresh-leaf-group-child" {
   try FreshLeafGroupChecks.crashChild(URL(fileURLWithPath: args[2], isDirectory: true), boundary: args[3])
   return
  }
  if args.count == 4 && args[1] == "listing-epoch-child" {
   try ListingEpochChecks.crashChild(URL(fileURLWithPath: args[2], isDirectory: true), boundary: args[3])
   return
  }
  let base=URL(fileURLWithPath:CommandLine.arguments[1],isDirectory:true).standardizedFileURL, fm=FileManager.default
  try checkRejectedStageFormats(base.appendingPathComponent("rejected-formats",isDirectory:true))
  try WriterCheckpointChecks.run(base.appendingPathComponent("writer-checkpoint",isDirectory:true))
  try FreshLeafGroupChecks.run(base.appendingPathComponent("fresh-leaf-groups",isDirectory:true))
  try FreshFileInsertChecks.run(base.appendingPathComponent("fresh-file-insert",isDirectory:true))
  try NoStaleTrimChecks.run(base.appendingPathComponent("no-stale-trim",isDirectory:true))
  try ListingEpochChecks.run(base.appendingPathComponent("listing-epochs",isDirectory:true))
  try checkDirectoryRegistrationRollback(base.appendingPathComponent("directory-rollback",isDirectory:true))
  try checkIdentityLookup(base.appendingPathComponent("identity",isDirectory:true))
  try checkLookupPreflight(base.appendingPathComponent("lookup",isDirectory:true))
  let root=base.appendingPathComponent("root",isDirectory:true).standardizedFileURL, cache=base.appendingPathComponent("cache",isDirectory:true).standardizedFileURL
  try fm.createDirectory(at:root,withIntermediateDirectories:true); try fm.createDirectory(at:cache,withIntermediateDirectories:true)
  let first=root.appendingPathComponent("z0000"); try Data(repeating:7,count:4096).write(to:first)
  for i in 1..<1500 { try fm.linkItem(at:first,to:root.appendingPathComponent(String(format:"z%04d",i))) }
  let checkpoint=StageCheckpoint(rootPath:root.path,identity:"test",eventID:0)
  var resumeDatabase = ""
  do {
   let stage=try ResumableStage(cache:cache,root:root,current:checkpoint)
   resumeDatabase = stage.lease.databasePath
   do { _ = try stage.crawler.step(beforeAccounting:{throw SimulatedCrash()}); fatalError("expected interruption") }
   catch is SimulatedCrash {}
  }
  var resume: OpaquePointer?
  guard sqlite3_open_v2(resumeDatabase, &resume, SQLITE_OPEN_READWRITE, nil) == SQLITE_OK else { fatalError("resume stage open") }
  guard sqlite3_exec(resume, "CREATE INDEX listing_identity ON nodes(dev,inode,is_dir,parent,name)", nil, nil, nil) == SQLITE_OK else { fatalError("resume stage index") }
  sqlite3_close(resume)
  let changed=StageCheckpoint(rootPath:root.path,identity:"replacement-journal",eventID:42)
  let stage=try ResumableStage(cache:cache,root:root,current:changed)
  guard sqlite3_open_v2(resumeDatabase, &resume, SQLITE_OPEN_READONLY, nil) == SQLITE_OK else { fatalError("resume stage reopen") }
  var resumeIndex: OpaquePointer?
  guard sqlite3_prepare_v2(resume, "SELECT 1 FROM sqlite_master WHERE name='listing_identity'", -1, &resumeIndex, nil) == SQLITE_OK,
        sqlite3_step(resumeIndex) == SQLITE_ROW else { fatalError("resume index removed during resume") }
  sqlite3_finalize(resumeIndex); sqlite3_close(resume)
  guard stage.checkpoint.identity == checkpoint.identity, stage.checkpoint.eventID == checkpoint.eventID else { throw NSError(domain:"original checkpoint changed",code:1) }
  func finish() throws { while try stage.advance(progress:{_ in}) {} }
  func verify(_ label:String) throws {
   let actual=try stage.view(path:nil,removals:[],cancellation:ScanCancellation())!.folder
   let expected=try DiskScanner().scan(root).root
   let byPath=Dictionary(uniqueKeysWithValues:actual.children.map {(Data($0.id.utf8),$0)})
   guard actual.allocatedSize == expected.allocatedSize, actual.size == expected.size,
         actual.fileCount == expected.fileCount, actual.children.count == expected.children.count else { throw NSError(domain:"totals \(label)",code:1) }
   for node in expected.children { guard byPath[Data(node.id.utf8)]?.allocatedSize == node.allocatedSize else { throw NSError(domain:"owner \(label) \(node.name)",code:1) } }
  }
  try finish(); try verify("resume candidates")
  let new=root.appendingPathComponent("A-first"); try fm.linkItem(at:first,to:new)
  try stage.crawler.markChanged(directory:root.path); try finish(); try verify("new earliest")
  try fm.removeItem(at:new)
  try stage.crawler.markChanged(directory:root.path); try finish(); try verify("deleted owner")
  try fm.removeItem(at:first); try Data(repeating:9,count:9000).write(to:first)
  try stage.crawler.markChanged(directory:root.path); try finish(); try verify("identity replacement")
  let handle=try FileHandle(forWritingTo:root.appendingPathComponent("z0001")); try handle.seekToEnd(); try handle.write(contentsOf:Data(repeating:3,count:9000)); try handle.close()
  try stage.crawler.markChanged(directory:root.path); try finish(); try verify("allocation change")
  let removalStart=ProcessInfo.processInfo.systemUptime
  for i in 1..<1400 { try fm.removeItem(at:root.appendingPathComponent(String(format:"z%04d",i))) }
  try stage.crawler.markChanged(directory:root.path); try finish(); try verify("bulk deletion")
  print("PASS: bulk removal of 1399 links reconciled in \(Int((ProcessInfo.processInfo.systemUptime-removalStart)*1000)) ms")
  let corruptRoot=base.appendingPathComponent("corrupt",isDirectory:true).standardizedFileURL, corruptCache=base.appendingPathComponent("corrupt-cache",isDirectory:true).standardizedFileURL
  try fm.createDirectory(at:corruptRoot,withIntermediateDirectories:true); try fm.createDirectory(at:corruptCache,withIntermediateDirectories:true)
  try Data([1]).write(to:corruptRoot.appendingPathComponent("item"))
  let corruptStore=DiskIndexStore(directory:corruptCache)
  let old=try corruptStore.save(DiskScanner().scan(corruptRoot),eventID:0,eventIdentity:"test")
  try Data("broken".utf8).write(to:old.databaseURL)
  let corruptStage=try ResumableStage(cache:corruptCache,root:corruptRoot,current:StageCheckpoint(rootPath:corruptRoot.path,identity:"test",eventID:0))
  while try corruptStage.advance(progress:{_ in}) {}
  let replacement=Data("newer cache contents".utf8); try replacement.write(to:old.databaseURL)
  do { _ = try corruptStage.publish(elapsed:0); fatalError("stale stage replaced corrupt cache") }
  catch SnapshotExpectation.Failure.changed {}
  guard try Data(contentsOf:old.databaseURL) == replacement else { throw NSError(domain:"corrupt cache replacement lost",code:1) }
  print("PASS: changed corrupt cache rejects stale publisher")
  print("PASS: interrupted candidate commit, additions, owner deletion, identity replacement and allocation changes")
  let excludedRoot=base.appendingPathComponent("exclude-root",isDirectory:true).standardizedFileURL
  let excludedCache=excludedRoot.appendingPathComponent("index",isDirectory:true), excludedStage=base.appendingPathComponent("separate-stage",isDirectory:true)
  try fm.createDirectory(at:excludedCache,withIntermediateDirectories:true)
  try Data([1]).write(to:excludedRoot.appendingPathComponent("kept"))
  let excluded=try ResumableStage(cache:excludedCache,stagingDirectory:excludedStage,root:excludedRoot,current:StageCheckpoint(rootPath:excludedRoot.path,identity:"test",eventID:0))
  try fm.moveItem(at:excludedCache,to:base.appendingPathComponent("old-index"))
  try fm.createDirectory(at:excludedCache,withIntermediateDirectories:true)
  try Data([9]).write(to:excludedCache.appendingPathComponent("internal"))
  while try excluded.advance(progress:{_ in}) {}
  let excludedView=try excluded.view(path:nil,removals:[],cancellation:ScanCancellation())!.folder
  guard excludedView.children.map(\.name) == ["kept"] else { throw NSError(domain:"replaced cache included in stage",code:1) }
  let refresh=try DiskScanner().scan(excludedRoot,excluding:[excludedCache,IndexLocations.locks(for:excludedCache),excludedStage])
  guard refresh.root.children.map(\.name) == ["kept"] else { throw NSError(domain:"replaced cache included in refresh",code:1) }
  print("PASS: replacing an excluded cache during mapping does not include its files")
  let removals=(0..<100000).map { ScanRemoval(path:root.appendingPathComponent("removed-\($0)").path,identity:"1:1") }
  for view in [false,true] {
   let token=ScanCancellation(), done=DispatchSemaphore(value:0), outcome=Outcome()
   DispatchQueue.global().async {
    defer {done.signal()}
    do { if view { _ = try stage.view(path:nil,removals:removals,cancellation:token) } else { _ = try stage.search(query:"z",cancellation:token,removals:removals,limit:300) } }
    catch is CancellationError { outcome.set() } catch { print("unexpected: \(error)") }
   }
   usleep(10000); let start=ProcessInfo.processInfo.systemUptime; token.cancel()
   guard done.wait(timeout:.now()+2) == .success, outcome.cancelled else { throw NSError(domain:"removal cancellation",code:1) }
   print("PASS: \(view ? "view" : "search") cancellation with 100k removals in \(Int((ProcessInfo.processInfo.systemUptime-start)*1000)) ms")
  }
 }
}
