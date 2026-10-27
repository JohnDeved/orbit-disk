import Foundation
import OrbitCore
@main struct Main {
 static func main() throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1]), root=base.appendingPathComponent("root"), cache=base.appendingPathComponent(UUID().uuidString)
  let stage=try ResumableStage(cache:cache,root:root,current:StageCheckpoint(rootPath:root.path,identity:"test",eventID:0))
  let store=DiskIndexStore(directory:cache), snapshot=stage.testPreviewSnapshot
  let rootChildren=try FileManager.default.contentsOfDirectory(atPath:root.path).count
  let deadline=ProcessInfo.processInfo.systemUptime+10
  // Test the partial SQLite snapshot while asynchronous discovery may already know descendants.
  while true {
   let committed=try store.stagedView(path:root.path,snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation())!.folder.children.count
   if committed == rootChildren {break}
   precondition(committed < rootChildren,"Root listing exceeded the fixture")
   precondition(ProcessInfo.processInfo.systemUptime < deadline,"Root listing did not reach the durable snapshot")
   let advanced=try stage.advance(progress:{_ in});precondition(advanced)
  }
  let firstView = try store.stagedView(path:root.path,snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation())!
  let firstSearch = try store.stagedSearch(query:"file",snapshot:snapshot,revision:1,removals:[],cancellation:ScanCancellation(),limit:10000)
  let expectedFirst = firstView.folder.children.filter { $0.name.hasPrefix("file") }
  precondition(firstSearch.count == expectedFirst.count)
  let expectedByPath = Dictionary(uniqueKeysWithValues: expectedFirst.map { (Data($0.id.utf8), $0) })
  for node in firstSearch.nodes {
   guard let expected = expectedByPath[Data(node.id.utf8)] else { fatalError("partial result missing in view") }
   precondition(node.scanState == expected.scanState && node.fileIdentity == expected.fileIdentity)
  }
  precondition(firstSearch.nodes.contains { $0.name == "file-folder" && $0.scanState == .pending })
  while try stage.advance(progress:{_ in}) {}
  func reference(_ query:String, removals:[ScanRemoval]) throws -> [DiskNode] {
   let locale=Locale(identifier:"en_US_POSIX")
   let needle=Array(query.trimmingCharacters(in:.whitespacesAndNewlines).folding(options:[.caseInsensitive,.diacriticInsensitive],locale:locale).utf8)
   guard !needle.isEmpty else{return []}
   var pending=[root.path], nodes:[DiskNode]=[]
   while let path=pending.popLast() {
    guard let view=try stage.view(path:path,removals:removals,cancellation:ScanCancellation()) else {fatalError("missing reference view")}
    for node in view.folder.children {
     if node.isDirectory { pending.append(node.id) }
     let name=node.name.folding(options:[.caseInsensitive,.diacriticInsensitive],locale:locale).utf8
     if name.indices.contains(where:{name[$0...].starts(with:needle)}) {nodes.append(node)}
    }
   }
   return nodes.sorted {a,b in a.size == b.size ? a.id.utf8.lexicographicallyPrecedes(b.id.utf8) : a.size > b.size}
  }
  func verify(_ query:String, _ removals:[ScanRemoval]) throws {
   let expected=try reference(query,removals:removals)
   for limit in [0,1,2,300,10000] {
    let actual=try stage.search(query:query,cancellation:ScanCancellation(),removals:removals,limit:limit)
    let wanted=Array(expected.prefix(limit))
    precondition(actual.count == expected.count)
    precondition(actual.nodes.map { Array($0.id.utf8) } == wanted.map { Array($0.id.utf8) })
    precondition(actual.nodes.map(\.fileIdentity) == wanted.map(\.fileIdentity))
    precondition(actual.nodes.map(\.size) == wanted.map(\.size))
    precondition(actual.nodes.map(\.allocatedSize) == wanted.map(\.allocatedSize))
    precondition(actual.nodes.map(\.fileCount) == wanted.map(\.fileCount))
    precondition(actual.nodes.map(\.scanState) == wanted.map(\.scanState))
   }
  }
  for query in ["file", "FÍLE", "file-0", "no-match"] {try verify(query,[])}
  let all=try reference("file",removals:[])
  let target=all.first { $0.name == "file-child" }!
  try verify("file",[ScanRemoval(path:target.id,identity:target.fileIdentity!)])
  try verify("file",[ScanRemoval(path:target.id,identity:"replacement")])
  print("PASS: independent folder traversal verifies limits, Unicode, ties, identities and removal overlays")
 }
}
