import Foundation
import Combine

final class Provider: @unchecked Sendable {
    private let lock = NSLock()
    private let gate = DispatchSemaphore(value: 0)
    private var n = 0
    var calls: Int { lock.lock(); defer { lock.unlock() }; return n }
    func run() -> [DiskVolume] {
        lock.lock(); n += 1; let call = n; lock.unlock()
        if call == 1 { gate.wait() }
        return [DiskVolume(url: URL(fileURLWithPath: "/Volumes/\(call)"), name: "v\(call)", capacity: Int64(call * 100), available: Int64(call * 10))]
    }
    func release() { gate.signal() }
}
final class OneShot: @unchecked Sendable {
    private let lock = NSLock()
    private let gate = DispatchSemaphore(value: 0)
    private var n = 0
    let result: [DiskVolume]
    init(_ result: [DiskVolume]) { self.result = result }
    var calls: Int { lock.lock(); defer { lock.unlock() }; return n }
    func run() -> [DiskVolume] {
        lock.lock(); n += 1; let call = n; lock.unlock()
        if call == 1 { gate.wait() }
        return result
    }
    func release() { gate.signal() }
}
@main struct VolumeChecks {
    @MainActor static func wait(_ label: String, _ ready: @escaping @MainActor () -> Bool) async throws {
        for _ in 0..<500 { if ready() { return }; try await Task.sleep(nanoseconds: 10_000_000) }
        fatalError("Timeout: \(label)")
    }
    @MainActor static func main() async throws {
        let base = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let p = Provider()
        let start = ProcessInfo.processInfo.systemUptime
        let m = DiskModel(cacheDirectory: base.appendingPathComponent("cache"), volumeDiscovery: { p.run() })
        let initTime = (ProcessInfo.processInfo.systemUptime - start) * 1000
        precondition(initTime < 100, "blocked discovery stalled initializer: \(initTime) ms")
        try await wait("first discovery started") { p.calls == 1 }
        precondition(m.isDiscoveringVolumes && m.volumes.count == 1 && m.volumes[0].url.path == "/" && m.volumes[0].name == "Mac"
                     && m.volumes[0].capacity == 0, "Mac must remain accessible while discovery is blocked")
        let refreshStart = ProcessInfo.processInfo.systemUptime
        for _ in 0..<50 { m.refreshVolumes() }
        let refreshTime = (ProcessInfo.processInfo.systemUptime - refreshStart) * 1000
        precondition(refreshTime < 100 && p.calls == 1, "refresh blocked or started unbounded workers")
        var reenter = true, observed: [[String]] = []
        let observer = m.objectWillChange.sink { _ in
            observed.append(m.volumes.map(\.name))
            if reenter { reenter = false; m.refreshVolumes() }
        }
        p.release()
        try await wait("newest volumes") { p.calls == 3 && m.volumes.map(\.name) == ["Mac", "v3"] }
        observer.cancel()
        precondition(!observed.contains(["Mac", "v1"]) && !observed.contains(["Mac", "v2"]), "stale result published")
        precondition(!m.isDiscoveringVolumes && m.volumes.last?.capacity == 300 && m.volumes.last?.available == 30)
        let actual = DiskVolume(url: URL(fileURLWithPath: "/"), name: "Actual Mac", capacity: 100, available: 40)
        let actualProvider = OneShot([actual])
        let actualModel = DiskModel(cacheDirectory: base.appendingPathComponent("real-cache"), volumeDiscovery: { actualProvider.run() })
        try await wait("actual root blocked") { actualProvider.calls == 1 }
        precondition(actualModel.isDiscoveringVolumes && actualModel.volumes[0].name == "Mac")
        actualProvider.release()
        try await wait("actual root replaces fallback") { !actualModel.isDiscoveringVolumes && actualModel.volumes[0].name == "Actual Mac" }
        precondition(actualModel.volumes.count == 1 && actualModel.volumes[0].available == 40)
        let emptyProvider = OneShot([])
        let emptyModel = DiskModel(cacheDirectory: base.appendingPathComponent("empty-cache"), volumeDiscovery: { emptyProvider.run() })
        try await wait("empty root blocked") { emptyProvider.calls == 1 }
        emptyProvider.release()
        try await wait("empty root finished") { !emptyModel.isDiscoveringVolumes }
        precondition(emptyModel.volumes.count == 1 && emptyModel.volumes[0].url.path == "/" && emptyModel.volumes[0].capacity == 0)
        var refreshReentry = true
        let refreshObserver = emptyModel.objectWillChange.sink { _ in
            if refreshReentry { refreshReentry = false; emptyModel.refreshVolumes() }
        }
        emptyModel.refreshVolumes()
        try await wait("reentrant restart") { emptyProvider.calls == 2 && !emptyModel.isDiscoveringVolumes }
        refreshObserver.cancel()
        precondition(!refreshReentry && emptyModel.volumes.count == 1 && emptyModel.volumes[0].url.path == "/")
        let p2 = Provider()
        var temporary: DiskModel? = DiskModel(cacheDirectory: base.appendingPathComponent("other-cache"), volumeDiscovery: { p2.run() })
        weak let weakModel = temporary
        try await wait("discovery for deallocation") { p2.calls == 1 }
        temporary = nil
        precondition(weakModel == nil, "blocked discovery retained model")
        p2.release()
        print(String(format:"PASS: blocked startup %.2f ms, 50 coalesced refreshes %.2f ms, stale/reentrant publication and deallocation",initTime,refreshTime))
    }
}
