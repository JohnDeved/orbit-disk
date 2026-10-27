import Foundation
import AppKit
import Combine
import OrbitCore
final class Gate: @unchecked Sendable {
 let lock=NSLock(), signal=DispatchSemaphore(value:0); var enteredValue=false
 var entered:Bool { lock.lock(); defer{lock.unlock()}; return enteredValue }
 func pause(){lock.lock();enteredValue=true;lock.unlock();signal.wait()}
 func release(){signal.signal()}
}
final class Outcome: @unchecked Sendable {let lock=NSLock();var value:String?;func set(_ v:String){lock.lock();value=v;lock.unlock()};var read:String?{lock.lock();defer{lock.unlock()};return value}}
final class Flag: @unchecked Sendable { let lock=NSLock();var value=0;func set(){lock.lock();value += 1;lock.unlock()};var read:Int{lock.lock();defer{lock.unlock()};return value}}
@main struct Main {
 @MainActor static func wait(_ message:String,_ check:()->Bool) async throws {for _ in 0..<500 {if check(){return};try await Task.sleep(nanoseconds:20_000_000)};throw NSError(domain:message,code:1)}
 @MainActor static func main() async throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1]),root=base.appendingPathComponent("root")
  try FileManager.default.createDirectory(at:root,withIntermediateDirectories:true)
  for i in 0..<3000 {FileManager.default.createFile(atPath:root.appendingPathComponent("file-\(i)").path,contents:Data())}
  let scanner=Gate(),search=Gate(),progress=Flag(),outcome=Outcome(),changes=Flag()
  var model:DiskModel? = DiskModel(cacheDirectory:base.appendingPathComponent("cache"),preferences:UserDefaults(suiteName:UUID().uuidString)!)
  model!.testScanner=DiskScanner(testBatch:{},testAfterListing:{scanner.pause()})
  model!.scan(root);try await wait("scan gate") {scanner.entered && model?.isPartialMap == true && model?.current?.children.count == 3000}
  model!.cancelScan();model!.testStopJournalMonitor();scanner.release()
  try await Task.sleep(nanoseconds:500_000_000)
  let subscription=model!.objectWillChange.sink {changes.set()}
  model!.testBeforePreviewSearch={search.pause()}
  model!.testAfterPreviewSearch={outcome.set($0)}
  DiskIndexStore.testSearchSQLiteProgress={progress.set()}
  model!.query="nonexistent";try await wait("search gate") {search.entered}
  let changesBeforeDrop=changes.read
  weak let weakModel=model
  model=nil
  try await wait("model deinit") {weakModel == nil}
  search.release()
  try await wait("worker finished") {outcome.read != nil}
  precondition(outcome.read == CommandLine.arguments[2], "Unexpected worker outcome: \(String(describing:outcome.read))")
  precondition(changes.read == changesBeforeDrop, "Search published after its model was dropped")
  withExtendedLifetime(subscription) {}
  print("model-deallocated=true sqlite-progress-callbacks-after-drop=\(progress.read) worker-outcome=\(outcome.read!) no-late-publication=true")
  DiskIndexStore.testSearchSQLiteProgress=nil
 }
}
