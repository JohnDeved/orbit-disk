import Foundation

struct DiscoveryDirectory {
    let path: Data
    let identity: DirectoryIdentity
    let components: [DirectoryComponent]
    var byteCount: Int { 128 + path.count + components.reduce(0) { $0 + 64 + $1.name.utf8.count } }
}

final class DiscoveryScheduler: @unchecked Sendable {
    struct Job {
        let ticket: Int64
        let directory: DiscoveryDirectory
    }
    private let condition = NSCondition()
    private let mailbox: PendingInventoryMailbox
    private var frontier: [DiscoveryDirectory]
    private var frontierBytes: Int
    private var active = 0
    private var nextTicket: Int64 = 0
    private var cancelled = false
    private var lastPreference: Data?
    private var checkPreference = false

    init(root: DiscoveryDirectory, mailbox: PendingInventoryMailbox) {
        self.mailbox = mailbox; frontier = [root]; frontierBytes = root.byteCount
        mailbox.updateFrontier(count: 1, bytes: frontierBytes)
    }

    func next(preferred: Data?) -> Job? {
        condition.lock(); defer { condition.unlock() }
        while frontier.isEmpty && active > 0 && !cancelled { condition.wait() }
        guard !cancelled, !mailbox.isCancelled, !frontier.isEmpty else { return nil }
        if preferred != lastPreference { lastPreference = preferred; checkPreference = true }
        var selected = frontier.count - 1
        if checkPreference, let preferred {
            if let index = frontier.firstIndex(where: { candidate in
                candidate.path == preferred || (preferred.starts(with: candidate.path)
                    && (candidate.path == Data([47]) || (preferred.count > candidate.path.count
                        && preferred[candidate.path.count] == 47)))
            }) {
                selected = index; checkPreference = frontier[index].path != preferred
            } else { checkPreference = false }
        }
        let directory = frontier.remove(at: selected)
        frontierBytes -= directory.byteCount; active += 1
        let job = Job(ticket: nextTicket, directory: directory); nextTicket += 1
        mailbox.updateFrontier(count: frontier.count, bytes: frontierBytes)
        return job
    }

    func complete(children: [DiscoveryDirectory]) {
        condition.lock(); defer { condition.unlock() }
        active -= 1
        if !cancelled {
            frontier.append(contentsOf: children)
            frontierBytes += children.reduce(0) { $0 + $1.byteCount }
            mailbox.updateFrontier(count: frontier.count, bytes: frontierBytes)
        }
        condition.broadcast()
    }

    func cancel() {
        condition.lock(); cancelled = true; frontier.removeAll(); frontierBytes = 0
        condition.broadcast(); condition.unlock()
    }
}

// Workers publish RAM independently. Only durable batches wait for an earlier directory's ticket.
final class DiscoveryListingSpool: @unchecked Sendable {
    private final class Listing {
        var batches: [PendingInventoryBatch?] = []
        var head = 0
        var closed = false
    }
    private let queue = DispatchQueue(label: "local.orbitdisk.discovery.sequence", qos: .userInitiated)
    private let mailbox: PendingInventoryMailbox
    private var listings: [Int64: Listing] = [:]
    private var nextTicket: Int64 = 0

    init(mailbox: PendingInventoryMailbox) { self.mailbox = mailbox }

    func append(_ batch: PendingInventoryBatch, ticket: Int64) {
        queue.async {
            guard !self.mailbox.isCancelled else { return }
            let listing = self.listing(ticket)
            listing.batches.append(batch)
            self.drain()
        }
    }

    func close(_ ticket: Int64) {
        queue.async {
            guard !self.mailbox.isCancelled else { return }
            self.listing(ticket).closed = true
            self.drain()
        }
    }

    func finish(_ completed: @escaping @Sendable () -> Void) {
        queue.async {
            self.drain()
            if !self.mailbox.isCancelled {
                if self.listings.isEmpty { self.mailbox.finish() }
                else { self.mailbox.cancel() }
            }
            self.listings.removeAll()
            completed()
        }
    }

    func cancel() { queue.async { self.listings.removeAll() } }

    private func listing(_ ticket: Int64) -> Listing {
        if let listing = listings[ticket] { return listing }
        let listing = Listing(); listings[ticket] = listing
        return listing
    }

    private func drain() {
        while !mailbox.isCancelled, let listing = listings[nextTicket] {
            while listing.head < listing.batches.count {
                guard let batch = listing.batches[listing.head] else { break }
                do { try mailbox.enqueue(batch) }
                catch { return }
                listing.batches[listing.head] = nil; listing.head += 1
            }
            guard listing.closed else { return }
            listings.removeValue(forKey: nextTicket); nextTicket += 1
        }
    }
}
