import Darwin
import Foundation
@main struct Check {
 static func main() throws {
  let args=CommandLine.arguments, fm=FileManager.default
  if args.count == 6 && args[1] == "child" {
   let cache=URL(fileURLWithPath:args[2],isDirectory:true), stage=URL(fileURLWithPath:args[3],isDirectory:true), root=URL(fileURLWithPath:args[4],isDirectory:true)
   _ = try ResumableStage(cache:cache,stagingDirectory:stage,root:root,current:StageCheckpoint(rootPath:root.standardizedFileURL.path,identity:"test",eventID:0))
   try Data([1]).write(to:URL(fileURLWithPath:args[5])); return
  }
  let base=URL(fileURLWithPath:args[1],isDirectory:true).standardizedFileURL
  let real=base.appendingPathComponent("real",isDirectory:true), alias=base.appendingPathComponent("alias",isDirectory:true)
  let root=base.appendingPathComponent("root",isDirectory:true), custom=base.appendingPathComponent("custom-stage",isDirectory:true)
  try fm.createDirectory(at:real,withIntermediateDirectories:true); try fm.createDirectory(at:root,withIntermediateDirectories:true)
  try fm.createSymbolicLink(at:alias,withDestinationURL:real)
  let realCache=real.appendingPathComponent("missing/cache",isDirectory:true), aliasCache=alias.appendingPathComponent("missing/cache",isDirectory:true)
  let before=DiskIndexStore(directory:realCache), aliased=DiskIndexStore(directory:aliasCache)
  guard before.directory.path == aliased.directory.path, before.lockDirectory.path == aliased.lockDirectory.path else { throw NSError(domain:"alias lock mismatch before creation",code:1) }
  try fm.createDirectory(at:realCache,withIntermediateDirectories:true)
  let after=DiskIndexStore(directory:aliasCache)
  guard after.lockDirectory.path == before.lockDirectory.path else { throw NSError(domain:"lock changed after cache creation: before=\(before.lockDirectory.path) after=\(after.lockDirectory.path)",code:1) }
  let home=fm.homeDirectoryForCurrentUser.standardizedFileURL.path, physical="/System/Volumes/Data"+home
  var a=stat(), b=stat()
  if stat(home,&a) == 0 && stat(physical,&b) == 0 && a.st_dev == b.st_dev && a.st_ino == b.st_ino {
   let suffix="/OrbitDisk-lock-alias-nonexistent/cache"
   guard IndexLocations.locks(for:URL(fileURLWithPath:home+suffix)).path == IndexLocations.locks(for:URL(fileURLWithPath:physical+suffix)).path else { throw NSError(domain:"data volume alias lock mismatch",code:1) }
  }
  let unsafe=base.appendingPathComponent("cache-link")
  try fm.createSymbolicLink(at:unsafe,withDestinationURL:realCache)
  do { _ = try DiskIndexStore(directory:unsafe).captureExpectedSnapshot(for:root); throw NSError(domain:"accepted cache symlink",code:1) }
  catch is DiskIndexError {}
  try fm.createDirectory(at:before.lockDirectory,withIntermediateDirectories:true)
  var lease:RootMutationLease? = try RootMutationLease(directory:before.lockDirectory,root:root)
  let marker=base.appendingPathComponent("acquired"), child=Process()
  child.executableURL=URL(fileURLWithPath:args[0]).standardizedFileURL
  child.arguments=["child",aliasCache.path,custom.path,root.path,marker.path]
  try child.run(); usleep(350000)
  guard child.isRunning, !fm.fileExists(atPath:marker.path) else { throw NSError(domain:"configured stage bypassed shared lock",code:1) }
  lease=nil
  child.waitUntilExit()
  guard child.terminationStatus == 0, fm.fileExists(atPath:marker.path) else { throw NSError(domain:"stage did not acquire released lock",code:1) }
  withExtendedLifetime(lease) {}
  print("PASS: aliases before and after cache creation share one cross-process root lock; cache symlink rejected")
 }
}
