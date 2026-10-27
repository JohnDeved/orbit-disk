import Foundation
import OrbitCore
@main struct Test {
 static func main() throws {
    let base = URL(fileURLWithPath: CommandLine.arguments[1]), root=base.appendingPathComponent("root"), outer=root.appendingPathComponent("match-outer"), nested=outer.appendingPathComponent("match-folder")
    let fm=FileManager.default
    try fm.createDirectory(at:nested,withIntermediateDirectories:true)
    for i in 0..<350 { try Data([1]).write(to:root.appendingPathComponent(String(format:"match-%03d",i))) }
    for i in 0..<20 { try Data([1,2]).write(to:nested.appendingPathComponent(String(format:"match-nested-%02d",i))) }
    let file=root.appendingPathComponent("match-000"), scanner=DiskScanner()
    let scan=try scanner.scan(root), oldStore=DiskIndexStore(directory:base.appendingPathComponent("old-index"))
    let oldSnapshot=try oldStore.save(scan,eventID:0,eventIdentity:"test")
    let savedBytes=try Data(contentsOf:oldSnapshot.databaseURL)
    let rootView=try oldStore.loadFolder(path:root.path,snapshot:oldSnapshot)
    let fileNode=rootView.children.first{$0.id == file.path}!, outerNode=rootView.children.first{$0.id == outer.path}!
    let nestedNode=try oldStore.loadFolder(path:outer.path,snapshot:oldSnapshot).children.first{$0.id == nested.path}!
    let removals=[ScanRemoval(path:file.path,identity:fileNode.fileIdentity!),ScanRemoval(path:nested.path,identity:nestedNode.fileIdentity!)]
    let projected=try oldStore.search(query:"match",snapshot:oldSnapshot,stablePathOrder:true,removals:removals)
    precondition(projected.totalCount == 350 && projected.nodes.count == 300)
    precondition(!projected.nodes.contains{$0.id == file.path || $0.id == nested.path || $0.id.hasPrefix(nested.path + "/")})
    let projectedOuter=try oldStore.search(query:"match-outer",snapshot:oldSnapshot,removals:removals).nodes.first!
    precondition(projectedOuter.id == outer.path && projectedOuter.size == 0 && projectedOuter.fileCount == 0 && outerNode.size == 40)
    try fm.removeItem(at:file);try fm.removeItem(at:nested)
    let fresh=try scanner.scan(root), freshStore=DiskIndexStore(directory:base.appendingPathComponent("fresh-index"))
    let freshSnapshot=try freshStore.save(fresh,eventID:0,eventIdentity:"test")
    let actual=try freshStore.search(query:"match",snapshot:freshSnapshot,stablePathOrder:true)
    precondition(projected.totalCount == actual.totalCount && projected.nodes.map{Array($0.id.utf8)} == actual.nodes.map{Array($0.id.utf8)})
    let mismatch=try oldStore.search(query:"match",snapshot:oldSnapshot,removals:[ScanRemoval(path:file.path,identity:"different")])
    precondition(mismatch.totalCount == 372)
    let unicodeRoot=base.appendingPathComponent("unicode-root"), unicodeFolder=unicodeRoot.appendingPathComponent("match-folder")
    let composedURL=URL(string:unicodeFolder.absoluteString + "/match-%C3%A9")!
    let decomposedURL=URL(string:unicodeFolder.absoluteString + "/match-e%CC%81")!
    let unicodeFiles=[DiskNode(url:composedURL,name:"match-é",isDirectory:false,size:1,allocatedSize:1,fileCount:1,fileIdentity:"unicode-0"), DiskNode(url:decomposedURL,name:"match-e\u{301}",isDirectory:false,size:1,allocatedSize:1,fileCount:1,fileIdentity:"unicode-1")]
    precondition(Array(unicodeFiles[0].id.utf8) != Array(unicodeFiles[1].id.utf8))
    let unicodeBranch=DiskNode(url:unicodeFolder,name:"match-folder",isDirectory:true,size:2,allocatedSize:2,fileCount:2,children:unicodeFiles,fileIdentity:"unicode-folder")
    let unicodeRemovals=unicodeFiles.map { ScanRemoval(path:$0.id,identity:$0.fileIdentity!) }
    let unicodeOne=DiskRemovalProjection(removals:Array(unicodeRemovals.prefix(1)),source:unicodeFiles).apply(unicodeBranch)!
    let unicodeBoth=DiskRemovalProjection(removals:unicodeRemovals,source:unicodeFiles).apply(unicodeBranch)!
    precondition(unicodeOne.size == 1 && unicodeOne.fileCount == 1 && unicodeOne.children.count == 1)
    precondition(unicodeBoth.size == 0 && unicodeBoth.fileCount == 0 && unicodeBoth.children.isEmpty)
    let composedFolder=URL(string:unicodeRoot.absoluteString + "/match-%C3%A9")!
    let composedChild=URL(string:composedFolder.absoluteString + "/child")!
    let composedLeaf=DiskNode(url:composedChild,name:"child",isDirectory:false,size:1,allocatedSize:1,fileCount:1,fileIdentity:"composed-child")
    let composedParent=DiskNode(url:composedFolder,name:"match-é",isDirectory:true,size:1,allocatedSize:1,fileCount:1,children:[composedLeaf],fileIdentity:"composed-parent")
    let nestedProjection=DiskRemovalProjection(removals:[ScanRemoval(path:composedLeaf.id,identity:"composed-child")],source:[composedLeaf])
    precondition(nestedProjection.apply(composedParent)?.size == 0, "Ancestor keys must preserve original UTF-8 bytes")
    let afterSearchBytes = try Data(contentsOf:oldSnapshot.databaseURL)
    precondition(afterSearchBytes == savedBytes, "Projected search must not modify the saved index")
    let cancel=ScanCancellation();cancel.cancel()
    do { _ = try oldStore.search(query:"match",snapshot:oldSnapshot,cancellation:cancel,removals:removals);preconditionFailure("missing cancellation") } catch is CancellationError {}
    print("PASS: saved search filters removed subtree and files, counts, projected totals, 300 cutoff, identity mismatch and cancellation")
 }
}
