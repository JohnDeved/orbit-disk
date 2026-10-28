import Foundation
import Darwin

public enum LocalAutomationError: Error, LocalizedError, Sendable {
    case unavailable
    case alreadyRunning
    case invalidEndpoint(String)
    case peerMismatch
    case oversizedMessage
    case timeout
    case disconnected
    case systemFailure(String, Int32)

    public var errorDescription: String? {
        switch self {
        case .unavailable: return "Orbit Disk is not running. Open the app and try again."
        case .alreadyRunning: return "Orbit Disk automation is already running."
        case .invalidEndpoint(let reason): return "The automation endpoint is unavailable: \(reason)"
        case .peerMismatch: return "The automation connection belongs to another user."
        case .oversizedMessage: return "Automation messages must not exceed 1 MiB."
        case .timeout: return "The automation request timed out."
        case .disconnected: return "The automation connection closed before the response was complete."
        case .systemFailure(let operation, let code): return "\(operation): \(String(cString: strerror(code)))"
        }
    }
}

public enum LocalAutomationEndpoint {
    public static var url: URL {
        let length = confstr(_CS_DARWIN_USER_TEMP_DIR, nil, 0)
        var bytes = [CChar](repeating: 0, count: max(1, length))
        let valid = confstr(_CS_DARWIN_USER_TEMP_DIR, &bytes, bytes.count) > 0
        return URL(fileURLWithPath: valid ? String(cString: bytes) : "/tmp", isDirectory: true)
            .appendingPathComponent("orbit-disk-automation-\(geteuid())", isDirectory: true)
            .appendingPathComponent("control.sock")
    }
}

public struct LocalAutomationClient: Sendable {
    public let endpoint: URL
    public init(endpoint: URL? = nil) { self.endpoint = endpoint ?? LocalAutomationEndpoint.url }

    public func request(_ payload: Data) throws -> Data {
        guard payload.count <= Wire.maximumSize else { throw LocalAutomationError.oversizedMessage }
        let directory = try EndpointDirectory(url: endpoint, create: false)
        defer { directory.close() }
        guard try directory.socketIdentity() != nil else { throw LocalAutomationError.unavailable }
        let fd = try Wire.socket()
        defer { Darwin.close(fd) }
        let deadline = Wire.deadline(seconds: 45)
        do { try Wire.connect(fd, endpoint: endpoint, deadline: deadline) }
        catch LocalAutomationError.systemFailure(_, let code) where code == ENOENT || code == ECONNREFUSED {
            throw LocalAutomationError.unavailable
        }
        try Wire.verifyPeer(fd)
        try Wire.writeFrame(payload, fd: fd, deadline: deadline)
        return try Wire.readFrame(fd: fd, deadline: deadline)
    }
}

public final class LocalAutomationServer: @unchecked Sendable {
    public let endpoint: URL
    private let handler: @Sendable (Data) async -> Data
    private let stateLock = NSLock()
    private var run: ServerRun?

    public init(endpoint: URL? = nil, handler: @escaping @Sendable (Data) async -> Data) {
        self.endpoint = endpoint ?? LocalAutomationEndpoint.url
        self.handler = handler
    }

    public func start() throws {
        stateLock.lock()
        defer { stateLock.unlock() }
        guard run == nil else { return }
        let created = try ServerRun(endpoint: endpoint, handler: handler)
        run = created
        created.start()
    }

    public func stop() {
        stateLock.lock()
        defer { stateLock.unlock() }
        run?.stop()
        run = nil
    }

    deinit { stop() }
}

private struct SocketIdentity: Equatable {
    let device: dev_t
    let inode: ino_t
}

private final class EndpointDirectory {
    let fd: Int32
    let name: String

    init(url: URL, create: Bool) throws {
        guard url.isFileURL, url.path.hasPrefix("/"), !url.lastPathComponent.isEmpty,
              !url.path.utf8.contains(0) else { throw LocalAutomationError.invalidEndpoint("invalid path") }
        let parent = url.deletingLastPathComponent().path
        if create, mkdir(parent, 0o700) != 0, errno != EEXIST {
            throw LocalAutomationError.systemFailure("Create automation directory", errno)
        }
        let opened = Darwin.open(parent, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC)
        guard opened >= 0 else {
            if !create, errno == ENOENT { throw LocalAutomationError.unavailable }
            throw LocalAutomationError.invalidEndpoint("directory is missing or is a symbolic link")
        }
        var info = stat()
        guard fstat(opened, &info) == 0, info.st_uid == geteuid(),
              info.st_mode & S_IFMT == S_IFDIR, info.st_mode & 0o777 == 0o700 else {
            Darwin.close(opened)
            throw LocalAutomationError.invalidEndpoint("directory must belong to this user and have mode 0700")
        }
        fd = opened
        name = url.lastPathComponent
        do { _ = try Wire.address(url) } catch { Darwin.close(opened); throw error }
    }

    func close() { Darwin.close(fd) }

    func lock() throws -> Int32 {
        let result = openat(fd, "control.lock", O_RDWR | O_CREAT | O_NOFOLLOW | O_CLOEXEC, 0o600)
        guard result >= 0 else { throw LocalAutomationError.invalidEndpoint("lock file is unavailable") }
        var info = stat()
        guard fstat(result, &info) == 0, info.st_uid == geteuid(), info.st_nlink == 1,
              info.st_mode & S_IFMT == S_IFREG, info.st_mode & 0o777 == 0o600 else {
            Darwin.close(result)
            throw LocalAutomationError.invalidEndpoint("lock file must be private and owned by this user")
        }
        guard flock(result, LOCK_EX | LOCK_NB) == 0 else {
            let code = errno
            Darwin.close(result)
            if code == EWOULDBLOCK { throw LocalAutomationError.alreadyRunning }
            throw LocalAutomationError.systemFailure("Lock automation endpoint", code)
        }
        return result
    }

    func socketIdentity() throws -> SocketIdentity? {
        var info = stat()
        guard fstatat(fd, name, &info, AT_SYMLINK_NOFOLLOW) == 0 else {
            if errno == ENOENT { return nil }
            throw LocalAutomationError.systemFailure("Read automation endpoint", errno)
        }
        guard info.st_uid == geteuid(), info.st_mode & S_IFMT == S_IFSOCK,
              info.st_mode & 0o777 == 0o600 else {
            throw LocalAutomationError.invalidEndpoint("socket must be private, owned by this user, and not a symbolic link")
        }
        return SocketIdentity(device: info.st_dev, inode: info.st_ino)
    }

    func unlinkSocket(matching expected: SocketIdentity) {
        guard (try? socketIdentity()) == expected else { return }
        _ = unlinkat(fd, name, 0)
    }
}

private final class Connection: @unchecked Sendable {
    let id = UUID()
    let fd: Int32
    private let lock = NSLock()
    private var closed = false
    private var interrupted = false
    private var task: Task<Void, Never>?

    init(_ fd: Int32) { self.fd = fd }
    func attach(_ task: Task<Void, Never>) {
        lock.lock(); defer { lock.unlock() }
        if closed || interrupted { task.cancel() } else { self.task = task }
    }
    func interrupt() {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return }
        interrupted = true
        task?.cancel()
        _ = shutdown(fd, SHUT_RDWR)
    }
    func close() {
        lock.lock(); defer { lock.unlock() }
        guard !closed else { return }
        closed = true
        task?.cancel(); task = nil
        Darwin.close(fd)
    }
}

private final class HandlerResult: @unchecked Sendable {
    let ready = DispatchSemaphore(value: 0)
    private let lock = NSLock()
    private var value: Data?
    func store(_ data: Data) { lock.lock(); value = data; lock.unlock(); ready.signal() }
    func load() -> Data? { lock.lock(); defer { lock.unlock() }; return value }
}

private final class ServerRun: @unchecked Sendable {
    private let directory: EndpointDirectory
    private let lockFD: Int32
    private let listener: Int32
    private let socketIdentity: SocketIdentity
    private let handler: @Sendable (Data) async -> Data
    private let state = NSLock()
    private var stopped = false
    private var listenerClosed = false
    private var connections: [UUID: Connection] = [:]
    private let finished = DispatchSemaphore(value: 0)

    init(endpoint: URL, handler: @escaping @Sendable (Data) async -> Data) throws {
        let directory = try EndpointDirectory(url: endpoint, create: true)
        var lockFD: Int32 = -1, listener: Int32 = -1
        var boundIdentity: SocketIdentity?
        do {
            lockFD = try directory.lock()
            if let old = try directory.socketIdentity() {
                let probe = try Wire.socket()
                defer { Darwin.close(probe) }
                do {
                    try Wire.connect(probe, endpoint: endpoint, deadline: Wire.deadline(seconds: 1))
                    throw LocalAutomationError.alreadyRunning
                } catch LocalAutomationError.systemFailure(_, let code) where code == ECONNREFUSED || code == ENOENT {
                    directory.unlinkSocket(matching: old)
                }
            }
            listener = try Wire.socket()
            var address = try Wire.address(endpoint)
            let bound = withUnsafePointer(to: &address) { pointer in
                pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                    Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
                }
            }
            guard bound == 0 else { throw LocalAutomationError.systemFailure("Bind automation socket", errno) }
            guard fchmodat(directory.fd, directory.name, 0o600, 0) == 0 else {
                throw LocalAutomationError.systemFailure("Protect automation socket", errno)
            }
            guard let identity = try directory.socketIdentity() else { throw LocalAutomationError.unavailable }
            boundIdentity = identity
            guard listen(listener, 8) == 0 else { throw LocalAutomationError.systemFailure("Listen for automation", errno) }
            self.directory = directory; self.lockFD = lockFD; self.listener = listener
            self.socketIdentity = identity; self.handler = handler
        } catch {
            if listener >= 0 { Darwin.close(listener) }
            if let boundIdentity { directory.unlinkSocket(matching: boundIdentity) }
            if lockFD >= 0 { Darwin.close(lockFD) }
            directory.close()
            throw error
        }
    }

    func start() { DispatchQueue(label: "orbit.automation.accept", qos: .utility).async { self.acceptLoop() } }

    func stop() {
        state.lock()
        stopped = true
        if !listenerClosed { _ = shutdown(listener, SHUT_RDWR) }
        for connection in connections.values { connection.interrupt() }
        state.unlock()
        finished.wait()
    }

    private var isStopped: Bool { state.lock(); defer { state.unlock() }; return stopped }

    private func acceptLoop() {
        defer {
            state.lock()
            stopped = true
            listenerClosed = true
            Darwin.close(listener)
            for connection in connections.values { connection.interrupt() }
            state.unlock()
            directory.unlinkSocket(matching: socketIdentity)
            Darwin.close(lockFD)
            directory.close()
            finished.signal()
        }
        while !isStopped {
            var descriptor = pollfd(fd: listener, events: Int16(POLLIN), revents: 0)
            let result = Darwin.poll(&descriptor, 1, 250)
            if result < 0 { if errno == EINTR { continue }; return }
            if result == 0 || isStopped { continue }
            let fd = Darwin.accept(listener, nil, nil)
            if fd < 0 { if errno == EAGAIN || errno == EINTR { continue }; return }
            do { try Wire.configure(fd); try Wire.verifyPeer(fd) }
            catch { Darwin.close(fd); continue }
            let connection = Connection(fd)
            state.lock()
            if stopped || connections.count >= 8 { state.unlock(); connection.close(); continue }
            connections[connection.id] = connection
            state.unlock()
            DispatchQueue.global(qos: .utility).async { self.serve(connection) }
        }
    }

    private func serve(_ connection: Connection) {
        defer {
            connection.close()
            state.lock(); connections.removeValue(forKey: connection.id); state.unlock()
        }
        do {
            let request = try Wire.readFrame(fd: connection.fd, deadline: Wire.deadline(seconds: 10))
            guard !isStopped else { return }
            let result = HandlerResult()
            let handler = handler
            let task = Task {
                guard !Task.isCancelled, !self.isStopped else { return }
                let data = await handler(request)
                guard !Task.isCancelled else { return }
                result.store(data)
            }
            connection.attach(task)
            let handlerDeadline = Wire.deadline(seconds: 30)
            while result.ready.wait(timeout: .now() + .milliseconds(100)) == .timedOut {
                if isStopped || DispatchTime.now().uptimeNanoseconds >= handlerDeadline { task.cancel(); return }
                var probe = pollfd(fd: connection.fd, events: Int16(POLLIN), revents: 0)
                if Darwin.poll(&probe, 1, 0) > 0 {
                    var byte: UInt8 = 0
                    if Darwin.recv(connection.fd, &byte, 1, MSG_PEEK) == 0 { task.cancel(); return }
                }
            }
            guard !isStopped, let response = result.load() else { return }
            try Wire.writeFrame(response, fd: connection.fd, deadline: Wire.deadline(seconds: 10))
        } catch { /* Malformed, abandoned, and timed-out connections have no application response. */ }
    }
}

private enum Wire {
    static let maximumSize = 1_048_576

    static func deadline(seconds: UInt64) -> UInt64 { DispatchTime.now().uptimeNanoseconds + seconds * 1_000_000_000 }

    static func socket() throws -> Int32 {
        let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { throw LocalAutomationError.systemFailure("Create automation socket", errno) }
        do { try configure(fd); return fd } catch { Darwin.close(fd); throw error }
    }

    static func configure(_ fd: Int32) throws {
        let flags = fcntl(fd, F_GETFL)
        var one: Int32 = 1
        guard flags >= 0, fcntl(fd, F_SETFL, flags | O_NONBLOCK) == 0,
              fcntl(fd, F_SETFD, FD_CLOEXEC) == 0,
              setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &one, socklen_t(MemoryLayout<Int32>.size)) == 0 else {
            throw LocalAutomationError.systemFailure("Configure automation socket", errno)
        }
    }

    static func address(_ endpoint: URL) throws -> sockaddr_un {
        var result = sockaddr_un()
        result.sun_family = sa_family_t(AF_UNIX)
        result.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        let bytes = Array(endpoint.path.utf8) + [0]
        guard bytes.count <= MemoryLayout.size(ofValue: result.sun_path) else {
            throw LocalAutomationError.invalidEndpoint("socket path is too long")
        }
        withUnsafeMutableBytes(of: &result.sun_path) { buffer in buffer.copyBytes(from: bytes) }
        return result
    }

    static func connect(_ fd: Int32, endpoint: URL, deadline: UInt64) throws {
        var value = try address(endpoint)
        let result = withUnsafePointer(to: &value) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        if result == 0 { return }
        guard errno == EINPROGRESS || errno == EINTR else {
            throw LocalAutomationError.systemFailure("Connect to Orbit Disk", errno)
        }
        try wait(fd, events: Int16(POLLOUT), deadline: deadline)
        var error: Int32 = 0
        var length = socklen_t(MemoryLayout<Int32>.size)
        guard getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &length) == 0 else {
            throw LocalAutomationError.systemFailure("Check automation connection", errno)
        }
        guard error == 0 else { throw LocalAutomationError.systemFailure("Connect to Orbit Disk", error) }
    }

    static func verifyPeer(_ fd: Int32) throws {
        var uid: uid_t = 0, gid: gid_t = 0
        guard getpeereid(fd, &uid, &gid) == 0 else { throw LocalAutomationError.systemFailure("Verify automation peer", errno) }
        guard uid == geteuid() else { throw LocalAutomationError.peerMismatch }
    }

    static func wait(_ fd: Int32, events: Int16, deadline: UInt64) throws {
        while true {
            let now = DispatchTime.now().uptimeNanoseconds
            guard now < deadline else { throw LocalAutomationError.timeout }
            let milliseconds = Int32(min(UInt64(Int32.max), (deadline - now + 999_999) / 1_000_000))
            var descriptor = pollfd(fd: fd, events: events, revents: 0)
            let result = Darwin.poll(&descriptor, 1, milliseconds)
            if result < 0 { if errno == EINTR { continue }; throw LocalAutomationError.systemFailure("Wait for automation", errno) }
            if result == 0 { throw LocalAutomationError.timeout }
            if descriptor.revents & Int16(POLLNVAL) != 0 { throw LocalAutomationError.disconnected }
            return
        }
    }

    static func readFrame(fd: Int32, deadline: UInt64) throws -> Data {
        let header = try read(4, fd: fd, deadline: deadline)
        let length = header.reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        guard length <= maximumSize else { throw LocalAutomationError.oversizedMessage }
        return try read(Int(length), fd: fd, deadline: deadline)
    }

    static func writeFrame(_ data: Data, fd: Int32, deadline: UInt64) throws {
        guard data.count <= maximumSize else { throw LocalAutomationError.oversizedMessage }
        var length = UInt32(data.count).bigEndian
        let header = withUnsafeBytes(of: &length) { Data($0) }
        try write(header, fd: fd, deadline: deadline)
        try write(data, fd: fd, deadline: deadline)
    }

    private static func read(_ count: Int, fd: Int32, deadline: UInt64) throws -> Data {
        var data = Data(count: count)
        try data.withUnsafeMutableBytes { bytes in
            var offset = 0
            while offset < count {
                try wait(fd, events: Int16(POLLIN), deadline: deadline)
                let result = Darwin.recv(fd, bytes.baseAddress!.advanced(by: offset), count - offset, 0)
                if result > 0 { offset += result; continue }
                if result == 0 { throw LocalAutomationError.disconnected }
                if errno == EINTR || errno == EAGAIN { continue }
                throw LocalAutomationError.systemFailure("Read automation message", errno)
            }
        }
        return data
    }

    private static func write(_ data: Data, fd: Int32, deadline: UInt64) throws {
        try data.withUnsafeBytes { bytes in
            var offset = 0
            while offset < data.count {
                try wait(fd, events: Int16(POLLOUT), deadline: deadline)
                let result = Darwin.send(fd, bytes.baseAddress!.advanced(by: offset), data.count - offset, 0)
                if result > 0 { offset += result; continue }
                if result == 0 { throw LocalAutomationError.disconnected }
                if errno == EINTR || errno == EAGAIN { continue }
                throw LocalAutomationError.systemFailure("Write automation message", errno)
            }
        }
    }
}
