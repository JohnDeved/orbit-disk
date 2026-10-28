import Foundation

func checkMappingProgress() throws {
    var cold = DiscoveryEstimator()
    var work = DiscoveryProgress()
    try require(cold.estimate(work, complete: false, now: 0).fraction == nil, "Empty discovery invented a total")
    work.files = 100_000; work.activeDirectories = 1
    try require(cold.estimate(work, complete: false, now: 0).fraction == nil,
                "A single unfinished flat listing invented its undiscovered entry count")
    work.files = 128; work.completedDirectories = 8; work.activeDirectories = 1
    work.queuedDirectories = 4; work.recentChildren = 0.2
    let first = cold.estimate(work, complete: false, now: 1)
    try require(first.fraction != nil && first.fraction! > 0 && first.fraction! < 1
        && first.basis == .currentScan, "Sampled discovery did not produce a bounded estimate")
    for fileCount in [0, 1, 63] {
        var sparse = DiscoveryEstimator()
        work.files = fileCount
        let sampled = sparse.estimate(work, complete: false, now: 1)
        try require(sampled.fraction == first.fraction && sampled.basis == .currentScan,
                    "Completed directory work stayed indeterminate solely because it contained few files")
    }
    work.files = 128
    work.queuedDirectories = 200
    let revised = cold.estimate(work, complete: false, now: 2)
    try require(revised.fraction! < first.fraction! && revised.basis == .revised,
                "Newly discovered work did not revise the estimate backwards")
    work.recentChildren = 1.2
    try require(cold.estimate(work, complete: false, now: 5).fraction == nil,
                "A growing frontier claimed a stable estimated total")

    var warm = DiscoveryEstimator(previousFileCount: 1_000)
    work.files = 500
    let previous = warm.estimate(work, complete: false, now: 0)
    try require(previous.fraction == 0.5 && previous.basis == .previousMap, "Previous complete map was ignored")
    work.files = 1_001
    let exceeded = warm.estimate(work, complete: false, now: 1)
    try require(exceeded.fraction == nil && exceeded.basis == .revised,
                "Outgrown previous map left an artificial 99% plateau")
    work.finished = true; work.uncertain = true
    try require(warm.estimate(work, complete: false, now: 2).fraction == nil,
                "Parallel-pass completion hid pending retries")
    let done = warm.estimate(work, complete: true, now: 3)
    try require(done.fraction == 1, "Confirmed completion was not exact")
    work.cancelled = true
    try require(warm.estimate(work, complete: true, now: 4).fraction == nil,
                "Cancellation was shown as successful completion")
    var emptyBaseline = DiscoveryEstimator(previousFileCount: 0)
    try require(emptyBaseline.estimate(DiscoveryProgress(), complete: false, now: 0).fraction == nil,
                "An empty baseline divided by zero")

    work = DiscoveryProgress(files: 9_000, acceptedSaveWork: 100, settledSaveWork: 25)
    try require(work.cacheFraction == 0.25, "Cache progress used discovered-file count as its denominator")
    work.settledSaveWork = 100
    try require(work.cacheFraction == 1, "Settled save-work fraction reserved an invented publication percentage")
    work.cacheUncertain = true
    try require(work.cacheFraction == nil, "Untracked imports retained a false cache denominator")
    var imported = DiscoveryEstimator(previousFileCount: 10_000)
    try require(imported.estimate(work, complete: false, now: 0).fraction == 0.9,
                "Cache import uncertainty hid known memory discovery progress")
    work.uncertain = true
    try require(work.cacheFraction == nil, "Unknown reuse/reconciliation work retained a false denominator")
    let legacy = ScanProgress(itemCount: 1, bytesScanned: 2, currentPath: "/fixture")
    try require(legacy.mapping == nil, "Legacy scanner acquired a fabricated mapping phase")
    print("PASS: discovery estimates revise, reject unknown totals, and separate cache work from file counts")
}
