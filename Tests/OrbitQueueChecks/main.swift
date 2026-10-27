import Foundation
import CSQLite
import Darwin

final class Outcome: @unchecked Sendable {
    let lock=NSLock()
    private var value: Error?
    func set(_ error: Error?) { lock.lock(); defer { lock.unlock() }; value=error }
    func get() -> Error? { lock.lock(); defer { lock.unlock() }; return value }
}

@main struct LazyChecks {
 static func main() throws {
    let path=CommandLine.arguments[1]
    if CommandLine.arguments.count == 3 && CommandLine.arguments[2] == "kill-during-migration" {
        let stage=try ListingStage(path:path)
        ListingStage.testAfterFirstQueueIndex = { kill(getpid(), SIGKILL) }
        try stage.prepareQueueIndexes(cancellation:ScanCancellation())
        fatalError("test child should have been killed")
    }
    let stage=try ListingStage(path:path)
    var db: OpaquePointer?
    guard sqlite3_open(path,&db)==SQLITE_OK else {fatalError("open")}
    defer { sqlite3_close(db) }
    func run(_ sql:String) { guard sqlite3_exec(db,sql,nil,nil,nil)==SQLITE_OK else {fatalError(String(cString:sqlite3_errmsg(db)))} }
    func scalar(_ sql:String) -> Int64 {
        var stmt:OpaquePointer?; guard sqlite3_prepare_v2(db,sql,-1,&stmt,nil)==SQLITE_OK else {fatalError("prepare")}
        defer { sqlite3_finalize(stmt) }
        guard sqlite3_step(stmt)==SQLITE_ROW else {fatalError("step")}; return sqlite3_column_int64(stmt,0)
    }
    run("BEGIN; WITH RECURSIVE seq(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM seq WHERE i<10000) INSERT INTO directories(path,device,inode,pending) SELECT CAST(printf('/%09d',i) AS BLOB),1,i,1 FROM seq; COMMIT")
    let child=Process()
    child.executableURL=URL(fileURLWithPath:CommandLine.arguments[0]).standardizedFileURL
    child.arguments=[path,"kill-during-migration"]
    try child.run(); child.waitUntilExit()
    guard child.terminationReason == .uncaughtSignal && child.terminationStatus == SIGKILL else {fatalError("migration child did not receive SIGKILL")}
    guard scalar("SELECT count(*) FROM directories")==10000 && scalar("SELECT count(*) FROM sqlite_master WHERE name IN ('pending_directories','pending_dirty')")==0 else {fatalError("crashed migration was not rolled back")}
    run("BEGIN")
    guard scalar("SELECT count(*) FROM directories")==10000 else {fatalError("fixture")}
    let token=ScanCancellation(), entered=DispatchSemaphore(value:0), release=DispatchSemaphore(value:0), finished=DispatchSemaphore(value:0), outcome=Outcome()
    ListingStage.testQueueMigrationProgress = { entered.signal(); release.wait() }
    DispatchQueue.global().async {
        do {try stage.prepareQueueIndexes(cancellation:token); outcome.set(nil)} catch {outcome.set(error)}
        finished.signal()
    }
    guard entered.wait(timeout:.now()+10) == .success else {fatalError("migration did not start")}
    guard scalar("SELECT count(*) FROM directories")==10000 else {fatalError("reader changed during migration")}
    token.cancel(); release.signal(); guard finished.wait(timeout:.now()+10) == .success else {fatalError("cancellation stalled")}
    ListingStage.testQueueMigrationProgress = nil
    guard outcome.get() is CancellationError else {fatalError("wrong cancellation result: \(String(describing:outcome.get()))")}
    run("COMMIT")
    guard scalar("SELECT count(*) FROM sqlite_master WHERE name IN ('pending_directories','pending_dirty')")==0 else {fatalError("partial migration committed")}
    run("BEGIN")
    guard scalar("SELECT count(*) FROM directories")==10000 else {fatalError("reader fixture")}
    try stage.prepareQueueIndexes(cancellation:ScanCancellation())
    guard scalar("SELECT count(*) FROM directories")==10000 else {fatalError("held reader failed")}
    run("COMMIT")
    guard scalar("SELECT count(*) FROM sqlite_master WHERE name IN ('pending_directories','pending_dirty')")==2 else {fatalError("retry failed")}
    let dirty=Data("/000005000".utf8)
    try stage.markDirty(parent:dirty)
    guard try stage.nextDirectory()?.path == dirty else {fatalError("dirty path lost priority")}
    run("BEGIN; UPDATE dirty SET pending=0; ROLLBACK")
    guard try stage.nextDirectory()?.path == dirty else {fatalError("rollback lost dirty priority")}
    print("PASS: SIGKILL during migration, active WAL reader, cancellation rollback, and retry")
 }
}
