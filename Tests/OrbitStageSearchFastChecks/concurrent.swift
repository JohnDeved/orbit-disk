import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(),sem=DispatchSemaphore(value:0);var entered=false,used=false
 var waiting:Bool {lock.lock();defer{lock.unlock()};return entered}
 func pause(){lock.lock();let block = !used;used=true;if block {entered=true};lock.unlock();if block {sem.wait()}}
 func release(){sem.signal()}
}
@main struct Main {
 static func main() async throws {
 let base=URL(fileURLWithPath:CommandLine.arguments[1]),root=base.appendingPathComponent("root"),cache=base.appendingPathComponent(UUID().uuidString)
 let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"bench",eventID:0))
 let store=DiskIndexStore(directory:cache), snapshot=stage.testPreviewSnapshot
 // Discovery can run ahead; establish the durable root boundary before pausing the reader.
 let deadline=ProcessInfo.processInfo.systemUptime+10
 while true {
  let committed=try store.stagedSearch(query:"file",snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation(),limit:0).count
  if committed == 2000 {break}
  precondition(committed < 2000,"Writer passed the root-only snapshot")
  precondition(ProcessInfo.processInfo.systemUptime < deadline,"Root listing did not reach the durable snapshot")
  let advanced=try stage.advance(progress:{_ in});precondition(advanced)
 }
 let before=try store.stagedSearch(query:"file",snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation(),limit:300)
 precondition(before.count == 2000)
 let prepared = try store.prepareStagedSearch(snapshot:snapshot); precondition(prepared)
 let gate=Gate();DiskIndexStore.testSearchSQLiteProgress={gate.pause()}
 let reader=Task.detached {try store.stagedSearch(query:"file",snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation(),limit:300)}
 for _ in 0..<5000 {if gate.waiting {break};try await Task.sleep(nanoseconds:1_000_000)}
 precondition(gate.waiting)
 let writer=Task.detached { while try stage.advance(progress:{_ in}) {} }
 try await writer.value
 gate.release();let during=try await reader.value;DiskIndexStore.testSearchSQLiteProgress=nil
 let after=try store.stagedSearch(query:"file",snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation(),limit:300)
 precondition(during.count == before.count && during.nodes.map{Array($0.id.utf8)} == before.nodes.map{Array($0.id.utf8)})
 precondition(after.count == 4000 && after.nodes.count == 300)
 print("PASS: search stays on one snapshot while mapping adds 2,000 matching files")
 }
}
