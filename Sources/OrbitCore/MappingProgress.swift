import Foundation

public struct MappingProgress: Equatable, Sendable {
    public enum EstimateBasis: Equatable, Sendable { case estimating, currentScan, previousMap, revised }

    public let revision: UInt64
    public let cachedFileCount: Int?
    public let discoveryFraction: Double?
    public let discoveryComplete: Bool
    public let estimateBasis: EstimateBasis
    public let cacheFraction: Double?
    public let cacheFinishing: Bool

    public init(revision: UInt64 = 0, cachedFileCount: Int? = nil, discoveryFraction: Double? = nil,
                discoveryComplete: Bool = false, estimateBasis: EstimateBasis = .estimating,
                cacheFraction: Double? = nil, cacheFinishing: Bool = false) {
        self.revision = revision; self.cachedFileCount = cachedFileCount
        self.discoveryFraction = discoveryFraction; self.discoveryComplete = discoveryComplete
        self.estimateBasis = estimateBasis; self.cacheFraction = cacheFraction; self.cacheFinishing = cacheFinishing
    }
}

struct DiscoveryProgress: Sendable {
    var files = 0
    var records = 0
    var logicalSize: Int64 = 0
    var completedDirectories = 0
    var activeDirectories = 0
    var queuedDirectories = 0
    var recentChildren: Double = 0
    var acceptedSaveWork: Int64 = 0
    var settledSaveWork: Int64 = 0
    var finished = false
    var uncertain = false
    var cacheUncertain = false
    var cancelled = false

    var cacheFraction: Double? {
        guard !cancelled, !uncertain, !cacheUncertain, acceptedSaveWork > 0 else { return nil }
        return min(1, Double(settledSaveWork) / Double(acceptedSaveWork))
    }
}

struct DiscoveryEstimator {
    var previousFileCount: Int?
    private var lastFraction: Double?
    private var revisedUntil: TimeInterval = 0

    init(previousFileCount: Int? = nil) {
        self.previousFileCount = previousFileCount.flatMap { $0 > 0 ? $0 : nil }
    }

    mutating func estimate(_ work: DiscoveryProgress, complete: Bool, now: TimeInterval)
        -> (fraction: Double?, basis: MappingProgress.EstimateBasis) {
        guard !work.cancelled else { return (nil, .estimating) }
        if complete { lastFraction = 1; return (1, .currentScan) }
        guard !work.uncertain else { return (nil, .estimating) }
        var fraction: Double?, basis: MappingProgress.EstimateBasis = .estimating
        if let previousFileCount, work.files < previousFileCount {
            fraction = min(0.98, Double(work.files) / Double(previousFileCount))
            basis = .previousMap
        } else {
            let remaining = work.activeDirectories + work.queuedDirectories
            // The recent branch rate can forecast unseen descendants, but a growing frontier has no stable total.
            if work.completedDirectories >= 8, remaining > 0, work.recentChildren < 0.9 {
                let remainingDirectories = Double(remaining) / max(0.1, 1 - work.recentChildren)
                let completed = Double(work.completedDirectories)
                fraction = min(0.98, completed / (completed + remainingDirectories))
                basis = .currentScan
            }
            if previousFileCount != nil { revisedUntil = now + 2 }
        }
        if let fraction, let previous = lastFraction, fraction < previous - 0.025 { revisedUntil = now + 2 }
        if let fraction { lastFraction = fraction }
        if now < revisedUntil { basis = .revised }
        return (fraction, basis)
    }
}
