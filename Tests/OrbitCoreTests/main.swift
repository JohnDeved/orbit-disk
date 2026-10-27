import Foundation

// The command-line tools do not ship XCTest; this runner keeps filesystem checks dependency-free.
struct CheckFailure: Error, CustomStringConvertible {
    let description: String
    init(_ description: String) { self.description = description }
}

class CheckCase {
    func setUpWithError() throws {}
    func tearDownWithError() throws {}
}

var failures = 0
var completed = 0
func checkTrue(_ value: @autoclosure () -> Bool, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
    if !value() { failures += 1; print("FAIL \(file):\(line) \(message)") }
}
func checkFalse(_ value: @autoclosure () -> Bool, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
    checkTrue(!value(), message, file: file, line: line)
}
func checkEqual<T: Equatable>(_ a: T, _ b: T, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) {
    checkTrue(a == b, "\(a) != \(b) \(message)", file: file, line: line)
}
func checkLessThan<T: Comparable>(_ a: T, _ b: T, file: StaticString = #filePath, line: UInt = #line) { checkTrue(a < b, file: file, line: line) }
func checkLessThanOrEqual<T: Comparable>(_ a: T, _ b: T, file: StaticString = #filePath, line: UInt = #line) { checkTrue(a <= b, file: file, line: line) }
func checkGreaterThanOrEqual<T: Comparable>(_ a: T, _ b: T, file: StaticString = #filePath, line: UInt = #line) { checkTrue(a >= b, file: file, line: line) }
func checkNil<T>(_ value: T?, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) { checkTrue(value == nil, message, file: file, line: line) }
func checkNotNil<T>(_ value: T?, _ message: String = "", file: StaticString = #filePath, line: UInt = #line) { checkTrue(value != nil, message, file: file, line: line) }
func checkUnwrap<T>(_ value: T?) throws -> T {
    guard let value else { throw CheckFailure("Expected non-nil value") }
    return value
}
func checkThrowsError<T>(_ expression: @autoclosure () throws -> T, file: StaticString = #filePath, line: UInt = #line, _ handler: (Error) -> Void = { _ in }) {
    do { _ = try expression(); checkTrue(false, "Expected an error", file: file, line: line) }
    catch { handler(error) }
}

func run<T: CheckCase>(_ name: String, make: () -> T, check: (T) throws -> Void) {
    let before = failures
    let test = make()
    do {
        try test.setUpWithError()
        defer {
            do { try test.tearDownWithError() }
            catch { failures += 1; print("FAIL cleanup: \(error)") }
        }
        try check(test)
    } catch { failures += 1; print("FAIL \(name): \(error)") }
    completed += 1
    if failures == before { print("PASS \(name)") }
}

let scannerChecks: [(String, (DiskScannerTests) throws -> Void)] = [
    ("Hierarchy, hidden files and empty folders", { try $0.testHierarchyIncludesHiddenFilesAndEmptyDirectories() }),
    ("Symlink loops and broken links", { try $0.testDoesNotFollowSymbolicLinks() }),
    ("Hard link allocation", { try $0.testHardLinksRetainLogicalSizeAndCountAllocationOnce() }),
    ("Sparse allocation", { try $0.testSparseFileSeparatesLogicalAndAllocatedSizes() }),
    ("Bulk metadata and fallback parity across batches", { try $0.testBulkMatchesFallbackAcrossMultipleBatches() }),
    ("Resource fork allocation parity", { try $0.testResourceForkAllocationMatchesFallback() }),
    ("Cancellation", { try $0.testCancellationBeforeAndDuringScan() }),
    ("Invalid scan roots", { try $0.testMissingFileRootAndSymlinkRootAreRejected() }),
    ("Permission errors", { try $0.testUnreadableChildIsReportedAndUnreadableRootThrows() }),
    ("Progress totals and throttling", { try $0.testProgressFinishesWithAccurateTotals() }),
    ("Progressive root, pending navigation and cancellation", { try $0.testPreviewPublishesRootAndPendingFoldersBeforeDescent() }),
    ("Progressive totals and bulk/fallback parity", { try $0.testPreviewTotalsMatchFinalScanAndFallback() }),
    ("Bounded progressive views and complete direct listings", { try $0.testPreviewHydrationIsBoundedButDirectChildrenRemainAvailable() }),
    ("Progressive empty and unavailable folders", { try $0.testPreviewDistinguishesUnavailableAndEmptyFolders() }),
    ("Progressive size ordering cache invalidation", { try $0.testPreviewOrderingInvalidatesWhenMeasuredChildSizesChange() })
]
for (name, check) in scannerChecks { run(name, make: DiskScannerTests.init, check: check) }
let policyChecks: [(String, (TrashPolicyTests) throws -> Void)] = [
    ("Trash root and sibling protection", { try $0.testOrdinaryDescendantIsAllowedButRootAndSiblingAreProtected() }),
    ("Trash symlink escape protection", { try $0.testSymlinkAndAncestorEscapeAreRejected() }),
    ("Trash hidden directory protection", { try $0.testHiddenDirectoriesTrashAndLibraryAreProtected() }),
    ("Trash system and missing path protection", { $0.testSystemPathsVolumesAndMissingItemsAreRejected() })
]
for (name, check) in policyChecks { run(name, make: TrashPolicyTests.init, check: check) }
let indexChecks: [(String, (DiskIndexStoreTests) throws -> Void)] = [
    ("Index replacement paths reject overlaps and remain cancellable", { try $0.testReplacementPathValidation() }),
    ("Index persistence and lazy folder queries", { try $0.testRoundTripAndLazyFolderQueries() }),
    ("Compact inventory structure, root paths, Unicode and identity bits", { try $0.testCompactInventoryPathsAndIdentityBits() }),
    ("Index filesystem NFC/NFD lookup and refresh", { try $0.testFilesystemUnicodeLookupAndRefresh() }),
    ("Index parent validation and atomic preservation", { try $0.testParentValidationPreservesSnapshot() }),
    ("Index preview across empty and nested folders", { try $0.testPreviewReusesQueryAcrossEmptyAndNestedFolders() }),
    ("Index literal search and ancestors", { try $0.testLiteralSearchAndAncestors() }),
    ("Index atomic cancellation and corrupt cache recovery", { try $0.testCancellationFailureAndInvalidCachesPreserveSnapshot() }),
    ("Index incremental create, delete, rename and totals", { try $0.testIncrementalCreateDeleteRenameAndTotals() }),
    ("Index incremental hard links across subtrees", { try $0.testIncrementalCrossSubtreeHardLinks() }),
    ("Index incremental hard links recalculate nested ancestors", { try $0.testIncrementalHardLinksRecalculateNestedAncestors() }),
    ("Index mixed hard-link batches match a full scan", { try $0.testIncrementalMixedHardLinkBatchMatchesFullScan() }),
    ("Index interrupted incremental update preserves snapshot", { try $0.testInterruptedIncrementalUpdatePreservesSnapshot() })
]
for (name, check) in indexChecks { run(name, make: DiskIndexStoreTests.init, check: check) }
print("\(completed) checks, \(failures) failures")
exit(failures == 0 ? 0 : 1)
