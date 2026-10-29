
extension ListingStage {
    func benchmarkProfile(_ profiler: SQLProfiler) {
        sqlite3_trace_v2(db, UInt32(SQLITE_TRACE_PROFILE), { _, context, rawStatement, elapsed in
            guard let context, let rawStatement, let elapsed else { return 0 }
            let profiler = Unmanaged<SQLProfiler>.fromOpaque(context).takeUnretainedValue()
            profiler.record(OpaquePointer(rawStatement), nanoseconds: elapsed.load(as: UInt64.self))
            return 0
        }, Unmanaged.passUnretained(profiler).toOpaque())
    }
}
