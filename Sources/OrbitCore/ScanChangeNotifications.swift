import Foundation

final class ScanChangeNotifications: @unchecked Sendable {
    private let lock = NSLock()
    private var subscribers: [UUID: AsyncStream<Void>.Continuation] = [:]

    func stream() -> AsyncStream<Void> {
        AsyncStream(bufferingPolicy: .bufferingNewest(1)) { continuation in
            let id = UUID()
            lock.lock(); subscribers[id] = continuation; lock.unlock()
            continuation.onTermination = { [weak self] _ in
                guard let self else { return }
                self.lock.lock(); self.subscribers.removeValue(forKey: id); self.lock.unlock()
            }
            continuation.yield(())
        }
    }

    func send() {
        lock.lock(); let targets = Array(subscribers.values); lock.unlock()
        for target in targets { target.yield(()) }
    }

    deinit { for subscriber in subscribers.values { subscriber.finish() } }
}
