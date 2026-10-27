import Foundation
import OrbitCore
import CSQLite
@main struct Main {
 static func main() throws {
 let base=URL(fileURLWithPath:CommandLine.arguments[1]),root=base.appendingPathComponent("root"),cache=base.appendingPathComponent(UUID().uuidString)
 let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"bench",eventID:0))
 while try stage.advance(progress:{_ in}) {}
 let directory=cache.appendingPathExtension("staging").appendingPathComponent(".orbit-stages")
 let file=try FileManager.default.contentsOfDirectory(at:directory,includingPropertiesForKeys:nil).first{ $0.pathExtension == "sqlite" }!
 var db:OpaquePointer?;precondition(sqlite3_open_v2(file.path,&db,SQLITE_OPEN_READWRITE,nil)==SQLITE_OK); defer {sqlite3_close(db)}
 for (column,original) in [("size",0),("allocated",0),("file_count",1),("raw_alloc",0)] {
  precondition(sqlite3_exec(db,"UPDATE nodes SET \(column)=-1 WHERE name='file-00000'",nil,nil,nil)==SQLITE_OK && sqlite3_changes(db)==1)
  for limit in [0,1,300] {
   do { _=try stage.search(query:"file",cancellation:ScanCancellation(),removals:[],limit:limit);fatalError("accepted damaged \(column) at limit \(limit)") } catch DiskIndexError.invalid { }
  }
  precondition(sqlite3_exec(db,"UPDATE nodes SET \(column)=\(original) WHERE name='file-00000'",nil,nil,nil)==SQLITE_OK)
 }
 print("PASS: four damaged numeric columns rejected even for limit 0 and discarded low-size matches")
 }
}
