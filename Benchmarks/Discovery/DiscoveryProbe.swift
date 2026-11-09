// Discovery-only probe for the Swift app (v0.9.15 sources), paired with the Rust
// `discovery_probe sample` (crates/orbit-bench/src/bin/discovery_probe.rs).
//
//   DiscoveryProbe producer ROOT WORKERS RECORDS FILES
//   DiscoveryProbe stage    ROOT NEW-CACHE-DIR RECORDS FILES
//
// `producer` runs `AsyncListingProducer` with a `LiveMappingInventory` as its batch hook and no
// writer, cache seed or reuse: the same work the Rust probe measures (parallel listing into the
// live inventory, a mailbox nobody drains). `stage` runs the real fresh `ResumableStage`
// discovery (four workers, SQLite seed reader per worker) with the writer held stopped, the
// "stage" mode of docs/benchmarks/parallel-discovery.json.
//
// Timing starts right before the producer (or `startDiscovery`) and ends at the mailbox's
// `discoveryFinishedAt`. CPU is this process's user/system time over the same span (read after
// the finish notification); peak RSS is the process maximum. One JSON line on stdout.
import Darwin
import Foundation

final class FinishSignal: @unchecked Sendable {
    let semaphore = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var mailbox: PendingInventoryMailbox?
    private var signalled = false

    func attach(_ mailbox: PendingInventoryMailbox) {
        lock.lock(); self.mailbox = mailbox; lock.unlock()
        check()
    }

    func check() {
        lock.lock()
        let fire = !signalled && (mailbox?.discoveryFinished ?? false)
        if fire { signalled = true }
        lock.unlock()
        if fire { semaphore.signal() }
    }
}

struct Usage {
    let userMs: Double
    let systemMs: Double
    let peakRSS: Int

    static func now() -> Usage {
        var usage = rusage()
        getrusage(RUSAGE_SELF, &usage)
        func ms(_ value: timeval) -> Double { Double(value.tv_sec) * 1000 + Double(value.tv_usec) / 1000 }
        return Usage(userMs: ms(usage.ru_utime), systemMs: ms(usage.ru_stime), peakRSS: Int(usage.ru_maxrss))
    }
}

@main struct DiscoveryProbe {
    static func main() throws {
        let arguments = CommandLine.arguments
        guard arguments.count == 6, ["producer", "stage"].contains(arguments[1]) else {
            FileHandle.standardError.write(Data("usage: DiscoveryProbe producer ROOT WORKERS RECORDS FILES | stage ROOT NEW-CACHE RECORDS FILES\n".utf8))
            exit(2)
        }
        let mode = arguments[1]
        let root = URL(fileURLWithPath: arguments[2], isDirectory: true).standardizedFileURL
        let expectedRecords = Int(arguments[4])!, expectedFiles = Int(arguments[5])!
        let signal = FinishSignal()
        let setupStarted = ProcessInfo.processInfo.systemUptime
        var workers = 4
        let mailbox: PendingInventoryMailbox
        let live: LiveMappingInventory
        let before: Usage
        let started: TimeInterval
        var keepAlive: [AnyObject] = []
        if mode == "producer" {
            workers = Int(arguments[3])!
            let cancellation = ScanCancellation()
            let secure = try CloudStoragePolicy.withoutMaterialization { try SecureRoot(url: root) }
            live = LiveMappingInventory(root: root, identity: secure.identity, cancellation: cancellation)
            before = Usage.now()
            started = ProcessInfo.processInfo.systemUptime
            let producer = try AsyncListingProducer(rootURL: root, rootIdentity: secure.identity,
                cancellation: cancellation, excludedDirectories: [], excludedPaths: [],
                deferDirectory: nil, preferredDirectory: nil, live: live, seedPath: nil,
                startingGeneration: 0, reuse: nil, forceFallback: false, workerCount: workers,
                onAccepted: { signal.check() })
            mailbox = producer.mailbox
            signal.attach(mailbox)
            producer.start()
            keepAlive.append(producer)
        } else {
            let cache = URL(fileURLWithPath: arguments[3], isDirectory: true).standardizedFileURL
            precondition(!FileManager.default.fileExists(atPath: cache.path), "the cache directory must be new")
            let checkpoint = StageCheckpoint(rootPath: root.path, identity: "discovery-benchmark", eventID: 0)
            let stage = try ResumableStage(cache: cache, root: root, current: checkpoint, cancellation: ScanCancellation())
            try stage.prepareRemovalReplay()
            live = stage.crawler.liveInventory
            before = Usage.now()
            started = ProcessInfo.processInfo.systemUptime
            try stage.startDiscovery(progress: { _ in signal.check() })
            guard let inventory = stage.crawler.asyncInventory else { fatalError("Expected parallel discovery") }
            mailbox = inventory
            signal.attach(mailbox)
            keepAlive.append(stage)
        }
        let setupMs = (started - setupStarted) * 1000
        while signal.semaphore.wait(timeout: .now() + 0.01) == .timedOut { signal.check() }
        let after = Usage.now()
        guard let finished = mailbox.discoveryFinishedAt else { fatalError("Discovery did not finish") }
        let discoveryMs = (finished - started) * 1000
        let progress = mailbox.progressSnapshot
        let liveFiles = try live.fileCount(removals: [], cancellation: ScanCancellation()) ?? -1
        let correct = progress.records == expectedRecords - 1 && progress.files == expectedFiles
            && liveFiles == expectedFiles && !progress.uncertain && !progress.cancelled
        let result: [String: Any] = [
            "mode": mode, "workers": workers,
            "discovery_ms": discoveryMs, "setup_ms": setupMs,
            "user_ms": after.userMs - before.userMs, "system_ms": after.systemMs - before.systemMs,
            "peak_rss": after.peakRSS, "pending_bytes": mailbox.pendingBytes,
            "records": progress.records, "files": progress.files, "live_files": liveFiles,
            "correct": correct,
        ]
        withExtendedLifetime(keepAlive) {}
        let data = try JSONSerialization.data(withJSONObject: result, options: [.sortedKeys])
        print(String(decoding: data, as: UTF8.self))
        guard correct else { exit(1) }
    }
}
