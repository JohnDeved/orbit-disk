import AppKit
import Foundation
import OrbitCore
@main struct ColorChecks {
 @MainActor static func main() async throws {
  let base=URL(fileURLWithPath:CommandLine.arguments[1]), root=base.appendingPathComponent("root")
  func file(_ name:String) -> DiskNode { DiskNode(url:root.appendingPathComponent(name),name:name,isDirectory:false,size:1,allocatedSize:1,fileCount:1) }
  func folder(_ nodes:[DiskNode]) -> DiskNode { DiskNode(url:root,name:"root",isDirectory:true,size:1,allocatedSize:1,fileCount:nodes.count,children:nodes) }
  let a=file("a"),b=file("b"),c=file("c"),d=file("d"),m=DiskModel(cacheDirectory:base.appendingPathComponent("cache"))
  try await m.testInstallColorFolder(folder([a,b]))
  precondition(m.childColorIndices[a.id] == 0 && m.childColorIndices[b.id] == 1)
  try await m.testPrepareDeferredColorFolder(folder([a,b,c]))
  try await m.testInstallColorFolder(folder([a,b,d]))
  precondition(m.testInstallDeferredColorFolder())
  precondition(m.childColorIndices[a.id] == 0 && m.childColorIndices[b.id] == 1 && m.childColorIndices[d.id] == 2 && m.childColorIndices[c.id] == 3,
               "A stale preparation must keep the newer colors and append its own")
  try await m.testPrepareDeferredColorFolder(folder([a,b,c,d]))
  let previousMapRevision = m.mapRevision, previousColorRevision = m.testColorMappingRevision
  try await m.testInstallColorFolder(folder([a,b,c,d]))
  precondition(m.mapRevision != previousMapRevision && m.testColorMappingRevision == previousColorRevision,
               "A no-op publication must change presentation revision but preserve color revision")
  precondition(m.testInstallDeferredColorFolder())
  precondition(m.childColorIndices[d.id] == 2 && m.childColorIndices[c.id] == 3,
               "A no-op publication must retain color assignments")
  try await m.testPrepareDeferredColorFolder(folder([a,b,c,d]))
  let other=base.appendingPathComponent("other"), x=DiskNode(url:other.appendingPathComponent("x"),name:"x",isDirectory:false,size:1,allocatedSize:1,fileCount:1)
  let otherFolder=DiskNode(url:other,name:"other",isDirectory:true,size:1,allocatedSize:1,fileCount:1,children:[x])
  let beforeSwitch=m.testColorMappingRevision
  try await m.testInstallColorFolder(otherFolder)
  precondition(m.testColorMappingRevision != beforeSwitch && m.childColorIndices[x.id] == 0)
  precondition(m.testInstallDeferredColorFolder())
  precondition(m.current?.id == root.path && m.childColorIndices[a.id] == 0 && m.childColorIndices[b.id] == 1 && m.childColorIndices[c.id] == 2 && m.childColorIndices[d.id] == 3 && m.childColorIndices[x.id] == nil,
               "A delayed result for a different folder must not reuse that folder's color map")
  try await m.testPrepareDeferredColorFolder(folder([a,b,c]))
  let beforeClear=m.testColorMappingRevision
  precondition(m.testClearSynthetic())
  precondition(m.testColorMappingRevision != beforeClear && m.childColorIndices.isEmpty)
  try await m.testInstallColorFolder(folder([b,a,d]))
  let reinstalledMap=m.mapRevision, reinstalledColors=m.testColorMappingRevision
  precondition(m.testInstallDeferredColorFolder())
  precondition(m.mapRevision != reinstalledMap && m.testColorMappingRevision != reinstalledColors && m.childColorIndices[b.id] == 0 && m.childColorIndices[a.id] == 1 && m.childColorIndices[d.id] == 2 && m.childColorIndices[c.id] == 3,
               "A delayed result after clearing and reinstalling must append to the new map")
  try await m.testPrepareDeferredColorFolder(folder([a,b,c]))
  let beforeSoftClear=m.testColorMappingRevision
  precondition(m.testClearPresentationKeepingColors())
  precondition(m.testColorMappingRevision == beforeSoftClear && m.childColorIndices[b.id] == 0)
  try await m.testInstallColorFolder(folder([d,b,a]))
  precondition(m.testColorMappingRevision != beforeSoftClear)
  precondition(m.testInstallDeferredColorFolder())
  precondition(m.childColorIndices[d.id] == 0 && m.childColorIndices[b.id] == 1 && m.childColorIndices[a.id] == 2 && m.childColorIndices[c.id] == 3,
               "A delayed result after presentation-only clearing must not overwrite the reinstalled map")
  print("PASS: map/color revisions, delayed additions, folder switches, clearing, and reinstalling")
 }
}
