import Foundation
import Darwin
import OrbitAutomation

private func require(_ value: @autoclosure () -> Bool, _ message: String) throws {
    if !value() { throw NSError(domain: "AutomationTransportChecks", code: 1, userInfo: [NSLocalizedDescriptionKey: message]) }
}

private final class Results: @unchecked Sendable {
    private let lock = NSLock()
    private var entries: [Result<Data, Error>] = []
    func append(_ entry: Result<Data, Error>) { lock.lock(); entries.append(entry); lock.unlock() }
    var all: [Result<Data, Error>] { lock.lock(); defer { lock.unlock() }; return entries }
}

private func rawSocket(_ endpoint: URL, bind: Bool = false) throws -> Int32 {
    let fd = socket(AF_UNIX, SOCK_STREAM, 0)
    guard fd >= 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
    var one: Int32 = 1
    _ = setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size))
    var address = sockaddr_un()
    address.sun_family = sa_family_t(AF_UNIX)
    address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
    let bytes = Array(endpoint.path.utf8) + [0]
    withUnsafeMutableBytes(of: &address.sun_path) { $0.copyBytes(from: bytes) }
    let result = withUnsafePointer(to: &address) { value in
        value.withMemoryRebound(to: sockaddr.self, capacity: 1) {
            bind ? Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                : Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    guard result == 0 else { let code = errno; close(fd); throw NSError(domain: NSPOSIXErrorDomain, code: Int(code)) }
    if bind { _ = chmod(endpoint.path, 0o600) }
    return fd
}

private func writeAll(_ bytes: Data, fd: Int32) throws {
    try bytes.withUnsafeBytes { buffer in
        var offset = 0
        while offset < bytes.count {
            let count = send(fd, buffer.baseAddress!.advanced(by: offset), bytes.count - offset, 0)
            if count < 0, errno == EINTR { continue }
            guard count > 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
            offset += count
        }
    }
}

private func readAll(_ count: Int, fd: Int32) throws -> Data {
    var result = Data(count: count)
    try result.withUnsafeMutableBytes { buffer in
        var offset = 0
        while offset < count {
            let read = recv(fd, buffer.baseAddress!.advanced(by: offset), count - offset, 0)
            if read < 0, errno == EINTR { continue }
            guard read > 0 else { throw NSError(domain: NSPOSIXErrorDomain, code: Int(errno)) }
            offset += read
        }
    }
    return result
}

@main
struct AutomationTransportChecks {
    static func main() throws {
        try checkIntegrationInstaller()
        let directory = URL(fileURLWithPath: "/tmp/oa-\(UUID().uuidString.prefix(8))", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        defer { try? FileManager.default.removeItem(at: directory) }
        let endpoint = directory.appendingPathComponent("control.sock")
        let server = LocalAutomationServer(endpoint: endpoint) { $0 }
        defer { server.stop() }
        let client = LocalAutomationClient(endpoint: endpoint)

        do { _ = try client.request(Data()); throw NSError(domain: "Expected unavailable", code: 1) }
        catch LocalAutomationError.unavailable {}
        try server.start()
        let request = Data("{\"path\":\"/tmp/é 🪐/\\nfile\",\"value\":9223372036854775807}".utf8)
        let reply = try client.request(request)
        try require(reply == request, "JSON and path bytes must survive roundtrip")
        let empty = try client.request(Data())
        try require(empty.isEmpty, "Empty frame")
        let maximum = Data(repeating: 0xa5, count: 1_048_576)
        let maximumReply = try client.request(maximum)
        try require(maximumReply == maximum, "1MiB frame")
        do { _ = try client.request(Data(repeating: 0, count: 1_048_577)); throw NSError(domain: "Expected oversized", code: 1) }
        catch LocalAutomationError.oversizedMessage {}
        print("PASS roundtrip, empty, maximum frame, client size limit")

        let second = LocalAutomationServer(endpoint: endpoint) { $0 }
        do { try second.start(); throw NSError(domain: "Expected lock", code: 1) }
        catch LocalAutomationError.alreadyRunning {}
        try require(FileManager.default.fileExists(atPath: endpoint.path), "Second server must preserve live endpoint")
        try server.start()
        print("PASS server lock and idempotent start")

        let splitFD = try rawSocket(endpoint)
        var size = UInt32(8193).bigEndian
        let header = withUnsafeBytes(of: &size) { Data($0) }
        for byte in header { try writeAll(Data([byte]), fd: splitFD) }
        let body = Data(repeating: 0x6b, count: 8193)
        for offset in stride(from: 0, to: body.count, by: 17) {
            try writeAll(body.subdata(in: offset..<min(body.count, offset + 17)), fd: splitFD)
        }
        let receivedHeader = try readAll(4, fd: splitFD)
        let receivedBody = try readAll(body.count, fd: splitFD)
        close(splitFD)
        try require(receivedHeader == header && receivedBody == body, "Fragmented frame")
        let oversizedFD = try rawSocket(endpoint)
        try writeAll(Data([0x7f, 0xff, 0xff, 0xff]), fd: oversizedFD)
        var probe = pollfd(fd: oversizedFD, events: Int16(POLLIN), revents: 0)
        try require(poll(&probe, 1, 2000) > 0, "Oversized frame should close promptly")
        var byte: UInt8 = 0
        try require(recv(oversizedFD, &byte, 1, 0) == 0, "Server must reject oversized frame before allocation")
        close(oversizedFD)
        print("PASS split I/O and malicious length rejection")

        server.stop()
        try require(!FileManager.default.fileExists(atPath: endpoint.path), "Stop removes owned socket")
        let staleFD = try rawSocket(endpoint, bind: true)
        close(staleFD)
        try server.start()
        let afterRestart = try client.request(request)
        try require(afterRestart == request, "Stale socket recovery")
        server.stop()
        let liveFD = try rawSocket(endpoint, bind: true)
        try require(listen(liveFD, 1) == 0, "Listen fixture")
        do { try server.start(); throw NSError(domain: "Expected live endpoint", code: 1) }
        catch LocalAutomationError.alreadyRunning {}
        try require(FileManager.default.fileExists(atPath: endpoint.path), "Do not unlink another live listener")
        close(liveFD)
        try FileManager.default.removeItem(at: endpoint)
        print("PASS stop, stale restart, unrelated live endpoint preservation")

        let target = directory.appendingPathComponent("target")
        try Data("keep".utf8).write(to: target)
        try FileManager.default.createSymbolicLink(at: endpoint, withDestinationURL: target)
        do { try server.start(); throw NSError(domain: "Expected symlink refusal", code: 1) }
        catch LocalAutomationError.invalidEndpoint {}
        try require((try? Data(contentsOf: target)) == Data("keep".utf8), "Symlink target unchanged")
        try FileManager.default.removeItem(at: endpoint)
        let lock = directory.appendingPathComponent("control.lock")
        try FileManager.default.removeItem(at: lock)
        try FileManager.default.createSymbolicLink(at: lock, withDestinationURL: target)
        do { try server.start(); throw NSError(domain: "Expected lock symlink refusal", code: 1) }
        catch LocalAutomationError.invalidEndpoint {}
        try FileManager.default.removeItem(at: lock)
        _ = chmod(directory.path, 0o755)
        do { try server.start(); throw NSError(domain: "Expected directory mode refusal", code: 1) }
        catch LocalAutomationError.invalidEndpoint {}
        _ = chmod(directory.path, 0o700)
        let alias = directory.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: directory)
        let aliasServer = LocalAutomationServer(endpoint: alias.appendingPathComponent("control.sock")) { $0 }
        do { try aliasServer.start(); throw NSError(domain: "Expected directory symlink refusal", code: 1) }
        catch LocalAutomationError.invalidEndpoint {}
        print("PASS socket, lock, directory symlink and permission rejection")

        let entered = DispatchSemaphore(value: 0)
        let pending = LocalAutomationServer(endpoint: endpoint) { _ in
            entered.signal()
            try? await Task.sleep(nanoseconds: 30_000_000_000)
            return Data("late".utf8)
        }
        try pending.start()
        let outcome = Results(), group = DispatchGroup()
        group.enter()
        DispatchQueue.global().async { outcome.append(Result { try client.request(request) }); group.leave() }
        try require(entered.wait(timeout: .now() + 2) == .success, "Pending request reached handler")
        let before = Date()
        pending.stop()
        try require(Date().timeIntervalSince(before) < 1, "Stop must not wait for handler completion")
        try require(group.wait(timeout: .now() + 2) == .success, "Stop disconnects pending client")
        if case .success = outcome.all.first { throw NSError(domain: "Cancelled handler must not respond", code: 1) }
        try server.start()
        let finalReply = try client.request(request)
        try require(finalReply == request, "Immediate restart after pending handler stop")
        print("PASS pending handler cancellation and immediate restart")

        let idle = try (0..<8).map { _ in try rawSocket(endpoint) }
        Thread.sleep(forTimeInterval: 0.15)
        let ninth = try rawSocket(endpoint)
        probe = pollfd(fd: ninth, events: Int16(POLLIN), revents: 0)
        try require(poll(&probe, 1, 2000) > 0, "Ninth connection is rejected")
        try require(recv(ninth, &byte, 1, 0) == 0, "Active connection bound")
        close(ninth)
        for fd in idle { close(fd) }
        Thread.sleep(forTimeInterval: 0.15)
        print("PASS eight-connection bound")

        let trickle = try rawSocket(endpoint)
        let start = Date()
        for part in [UInt8(0), 0, 0, 1] {
            try writeAll(Data([part]), fd: trickle)
            if Date().timeIntervalSince(start) < 8 { Thread.sleep(forTimeInterval: 3) }
        }
        probe = pollfd(fd: trickle, events: Int16(POLLIN), revents: 0)
        try require(poll(&probe, 1, 3000) > 0, "Trickle client must not extend total frame deadline")
        try require(recv(trickle, &byte, 1, 0) == 0, "Trickle client closed at total read deadline")
        try require(Date().timeIntervalSince(start) < 12, "Absolute read deadline")
        close(trickle)
        print("PASS whole-frame deadline under trickle input")
        print("All automation transport checks passed")
    }
}
