import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(), sem=DispatchSemaphore(value:0); var waitingValue=false, used=false
 var waiting:Bool { lock.lock(); defer{lock.unlock()}; return waitingValue }
 func pause(){ lock.lock(); let block = !used; used=true; if block { waitingValue=true };lock.unlock();if block {sem.wait()} }
 func release(){sem.signal()}
}
@main struct Main {
 static func main() async throws {
 DiskIndexStore.testDisablePreparedSearch = true
 defer { DiskIndexStore.testDisablePreparedSearch = false }
 let base=URL(fileURLWithPath:CommandLine.arguments[1]),root=base.appendingPathComponent("root"),cache=base.appendingPathComponent(UUID().uuidString)
 let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"bench",eventID:0))
 stage.testDisableAsyncDiscovery = true
 while try stage.advance(progress:{_ in}) {}
 for cancelTask in [true,false] {
  let gate=Gate(), token=ScanCancellation()
  DiskIndexStore.testSearchSQLiteProgress = { gate.pause() }
  let worker=Task.detached { try stage.search(query:"no-such-filename",cancellation:token,removals:[],limit:300) }
  for _ in 0..<5000 { if gate.waiting {break}; try await Task.sleep(nanoseconds:1_000_000) }
  precondition(gate.waiting,"SQLite progress handler not reached")
  if cancelTask { worker.cancel() } else { token.cancel() }
  gate.release()
  do { _=try await worker.value; fatalError("search did not cancel") } catch is CancellationError {}
  DiskIndexStore.testSearchSQLiteProgress=nil
  print("PASS: \(cancelTask ? "Task" : "token") cancellation interrupts an executing SQLite search")
 }
 let result=try stage.search(query:"file",cancellation:ScanCancellation(),removals:[],limit:300)
 precondition(result.count == 20000 && result.nodes.count == 300)
 print("PASS: subsequent search still works")
 }
}
