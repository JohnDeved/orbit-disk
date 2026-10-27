import AppKit
import Combine
import Foundation
import OrbitCore

@main struct Check {
    @MainActor static func main() throws {
        let url = URL(fileURLWithPath: CommandLine.arguments[1])
        let model = DiskModel(cacheDirectory: url.appendingPathComponent("cache"), preferences: UserDefaults(suiteName: UUID().uuidString)!)
        let nodes = (0..<5_000).map { index in
            DiskNode(url: url.appendingPathComponent("f\(index)"), name: "f\(index)", isDirectory: false,
                     size: 1, allocatedSize: 1, fileCount: 1, fileIdentity: "1:\(index)")
        }
        func root(_ children: [DiskNode]) -> DiskNode {
            DiskNode(url: url, name: "root", isDirectory: true, size: Int64(children.count),
                     allocatedSize: Int64(children.count), fileCount: children.count, children: children, fileIdentity: "1:root")
        }
        precondition(model.testInstallSynthetic(root(nodes), selection: nodes[0].id))
        model.hoveredMapNodes = [nodes[0]]; model.hoveredListID = nodes[0].id
        precondition(model.testInstallSynthetic(root(nodes), selection: nodes[0].id))
        precondition(model.hoveredMapNodes.first?.id == nodes[0].id && model.hoveredListID == nodes[0].id && model.selectedID == nodes[0].id)
        var calls = 0
        model.largeListWillReset = { calls += 1; model.selectedID = "selection-changed-in-callback" }
        let accepted = model.testInstallSynthetic(root(Array(nodes.reversed())))
        precondition(!accepted && calls == 1 && model.current?.children.first?.id == nodes[0].id)
        precondition(model.selectedID == "selection-changed-in-callback")
        model.largeListWillReset = nil
        let second = DiskNode(url: url.appendingPathComponent("second"), name: "second", isDirectory: true,
                              size: 0, allocatedSize: 0, fileCount: 0, fileIdentity: "1:second")
        var switchOnClear = true
        let subscription = model.objectWillChange.sink {
            MainActor.assumeIsolated {
                if switchOnClear {
                    switchOnClear = false
                    precondition(model.testInstallSynthetic(second))
                }
            }
        }
        precondition(!model.testClearSynthetic())
        precondition(model.root?.id == second.id && model.current?.id == second.id)
        withExtendedLifetime(subscription) {}
        print("same-folder hover/selection retained; reentrant list-reset rejected; reentrant root clear rejected")
    }
}
