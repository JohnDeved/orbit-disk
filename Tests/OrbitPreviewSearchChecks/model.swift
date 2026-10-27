import AppKit
import Combine
import Foundation
import OrbitCore
final class Gate: @unchecked Sendable {
    private let lock = NSLock(), signal = DispatchSemaphore(value: 0)
    private var state = false, used = false
    var waiting: Bool { lock.lock(); defer { lock.unlock() }; return state }
    func pause() { lock.lock(); let block = !used; used = true; if block { state = true }; lock.unlock(); if block { signal.wait() } }
    func release() { signal.signal() }
}
final class OrderRecorder: @unchecked Sendable {
    private let lock = NSLock(); private var values: [Bool] = []
    func record(_ value: Bool) { lock.lock(); values.append(value); lock.unlock() }
    var recorded: [Bool] { lock.lock(); defer { lock.unlock() }; return values }
}
@main struct Test {
    @MainActor static func wait(_ name: String, _ predicate: () -> Bool) async throws {
        for _ in 0..<500 { if predicate() { return }; try await Task.sleep(nanoseconds: 20_000_000) }
        throw NSError(domain: name, code: 1)
    }
    @MainActor static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1]), root = base.appendingPathComponent("root"), nested = root.appendingPathComponent("sub")
        try FileManager.default.createDirectory(at: nested, withIntermediateDirectories: true)
        let early = root.appendingPathComponent("café-early"), later = nested.appendingPathComponent("cafe-later")
        try Data([1]).write(to: early); try Data([2,3]).write(to: later)
        let gate = Gate(), prefs = UserDefaults(suiteName: "search-check-\(UUID())")!
        let model = DiskModel(cacheDirectory: base.appendingPathComponent("index"), preferences: prefs, scanProgressObserver: { if $0.currentPath == root.path && $0.itemCount > 0 { gate.pause() } })
        model.scan(root); try await wait("partial preview") { gate.waiting && model.current?.children.count == 2 }
        model.query = "CAFE"
        try await wait("partial search") { !model.isSearching && model.searchResults.contains { $0.id == early.path } }
        precondition(model.isPartialMap && model.query == "CAFE")
        let first = model.searchResults.first { $0.id == early.path }!
        precondition(model.collect([first])); model.deleteCollectionPermanently()
        try await wait("delete during search") { !model.isRemoving && !FileManager.default.fileExists(atPath: early.path) }
        try await wait("search projection") { !model.searchResults.contains { $0.id == early.path } }
        precondition(model.query == "CAFE" && gate.waiting)
        gate.release(); try await wait("finished search") { !model.isScanning && !model.isPartialMap && !model.isSearching && model.searchResults.contains { $0.id == later.path } }
        precondition(model.query == "CAFE" && !model.searchResults.contains { $0.id == early.path })
        model.cancelScan()
        let wide = base.appendingPathComponent("wide")
        try FileManager.default.createDirectory(at: wide, withIntermediateDirectories: true)
        for i in 0..<2_000 { try Data([1]).write(to: wide.appendingPathComponent(String(format: "entry-%04d", i))) }
        let batchGate = Gate(), progressGate = Gate()
        let wideModel = DiskModel(cacheDirectory: base.appendingPathComponent("wide-index"), preferences: prefs)
        wideModel.testScanner = DiskScanner(testBatch: { batchGate.pause() }, testAfterListing: { progressGate.pause() })
        wideModel.scan(wide); try await wait("wide first batch") { batchGate.waiting && !(wideModel.current?.children.isEmpty ?? true) }
        let visible = Set(wideModel.current!.children.map(\.name))
        let missing = (0..<2_000).map { String(format: "entry-%04d", $0) }.first { !visible.contains($0) }!
        let initiallyVisible = visible.first!
        wideModel.query = initiallyVisible
        try await wait("wide initial match") { !wideModel.isSearching && wideModel.searchResults.contains { $0.name == initiallyVisible } }
        let refreshGate = Gate(); wideModel.testBeforePreviewSearch = { refreshGate.pause() }
        batchGate.release(); try await wait("wide listing complete") { progressGate.waiting && wideModel.isPartialMap }
        try await wait("wide refresh held") { refreshGate.waiting }
        precondition(wideModel.searchResults.contains { $0.name == initiallyVisible }, "same-query refresh must preserve visible results")
        wideModel.testBeforePreviewSearch = {}; refreshGate.release()
        wideModel.query = missing
        try await wait("wide progressive match") { wideModel.isPartialMap && wideModel.searchResults.contains { $0.name == missing } }
        precondition(wideModel.query == missing && progressGate.waiting)
        var armed = false, immediateStateCorrect = false
        let priorRows = wideModel.searchResults.map { Array($0.id.utf8) }
        let observer = wideModel.objectWillChange.sink {
            if armed && wideModel.query == "entry-0000" {
                armed = false; wideModel.query = "entry-0001"
                immediateStateCorrect = wideModel.isSearching && wideModel.searchResults.map { Array($0.id.utf8) } == priorRows
            }
        }
        wideModel.query = "entry-0000"; armed = true
        try await wait("reentrant search change") { wideModel.query == "entry-0001" && !wideModel.isSearching && wideModel.searchResults.contains { $0.name == "entry-0001" } }
        precondition(immediateStateCorrect)
        withExtendedLifetime(observer) {}
        let other = base.appendingPathComponent("other")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        try Data([1]).write(to: other.appendingPathComponent("other-file"))
        var switching = false, switchedInsideNotification = false
        let switchObserver = wideModel.objectWillChange.sink {
            if switching && wideModel.query == "entry-0000" {
                switching = false; wideModel.cancelScan(); wideModel.scan(other)
                switchedInsideNotification = true
            }
        }
        wideModel.query = "entry-0000"; switching = true
        try await wait("search root switch") { switchedInsideNotification && wideModel.root?.id == other.path && !wideModel.isScanning }
        precondition(wideModel.searchResults.allSatisfy { $0.id.hasPrefix(other.path + "/") })
        withExtendedLifetime(switchObserver) {}
        progressGate.release(); wideModel.cancelScan(); print("PASS: partial results refresh before scan completion")
        let orderRoot = base.appendingPathComponent("order")
        try FileManager.default.createDirectory(at: orderRoot, withIntermediateDirectories: true)
        for i in 0..<1_100 { try Data([1]).write(to: orderRoot.appendingPathComponent(String(format: "match-%04d", i))) }
        let orderGate = Gate(), orderRecorder = OrderRecorder()
        let orderModel = DiskModel(cacheDirectory: base.appendingPathComponent("order-index"), preferences: prefs)
        orderModel.testScanner = DiskScanner(testBatch: {}, testAfterListing: { orderGate.pause() })
        orderModel.testSavedSearchOrder = { orderRecorder.record($0) }
        orderModel.scan(orderRoot); try await wait("order listing") { orderGate.waiting && orderModel.current?.children.count == 1_100 }
        orderModel.query = "match-"
        try await wait("order partial results") { !orderModel.isSearching && orderModel.searchResults.count == 300 }
        let beforeHandoff = orderModel.searchResults.map { Array($0.id.utf8) }
        orderGate.release(); try await wait("order saved results") { !orderModel.isScanning && !orderModel.isPartialMap && !orderModel.isSearching && !orderRecorder.recorded.isEmpty }
        precondition(orderRecorder.recorded.last == true && orderModel.searchResults.map { Array($0.id.utf8) } == beforeHandoff)
        let previousSearches = orderRecorder.recorded.count
        orderModel.rescan(); try await wait("order saved refresh") { orderRecorder.recorded.count > previousSearches && !orderModel.isRefreshing && !orderModel.isSearching }
        precondition(orderRecorder.recorded.last == true && orderModel.searchResults.map { Array($0.id.utf8) } == beforeHandoff)
        orderModel.query = "match-0001"
        try await wait("ordinary saved order") { !orderModel.isSearching && orderRecorder.recorded.last == false && !orderModel.searchResults.isEmpty }
        orderModel.cancelScan(); print("PASS: preview tie order survives saved handoff and refresh; later query uses indexed order")
        print("PASS: partial search stays current through deletion and saved-index handoff")
    }
}
