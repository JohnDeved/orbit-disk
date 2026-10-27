import AppKit
import SwiftUI
import OrbitCore

private struct Pixels {
    let width: Int
    let height: Int
    let scale: CGFloat
    let bytes: [UInt8]

    init(image: CGImage, scale: CGFloat) {
        width = image.width
        height = image.height
        self.scale = scale
        var bytes = [UInt8](repeating: 0, count: image.width * image.height * 4)
        bytes.withUnsafeMutableBytes { buffer in
            let context = CGContext(data: buffer.baseAddress, width: image.width, height: image.height,
                                    bitsPerComponent: 8, bytesPerRow: image.width * 4,
                                    space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                    bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
            context.draw(image, in: CGRect(x: 0, y: 0, width: image.width, height: image.height))
        }
        self.bytes = bytes
    }

    func rgb(at point: CGPoint) -> [Int] {
        let x = min(width - 1, max(0, Int(point.x * scale)))
        let y = min(height - 1, max(0, Int(point.y * scale)))
        let offset = (y * width + x) * 4
        return bytes[offset..<offset + 3].map(Int.init)
    }

    func difference(from other: Self, at point: CGPoint) -> Int {
        zip(rgb(at: point), other.rgb(at: point)).map { abs($0 - $1) }.max()!
    }
}

@main struct OrbitRenderChecks {
    @MainActor static func main() async throws {
        NSApplication.shared.setActivationPolicy(.prohibited)
        var failures: [String] = []
        var checks = 0
        func check(_ condition: Bool, _ message: String) {
            checks += 1
            if !condition { failures.append(message); print("FAIL: \(message)") }
        }
        func node(_ path: String, size: Int64 = 100, children: [DiskNode] = []) -> DiskNode {
            DiskNode(url: URL(fileURLWithPath: path), name: URL(fileURLWithPath: path).lastPathComponent,
                     isDirectory: !children.isEmpty, size: children.isEmpty ? size : children.reduce(0) { $0 + $1.size },
                     allocatedSize: size, fileCount: children.isEmpty ? 1 : children.count, children: children)
        }
        let branches = (0..<4).map { branch in
            node("/render-fixture/branch-\(branch)", children: (0..<3).map { leaf in
                node("/render-fixture/branch-\(branch)/file-\(leaf)")
            })
        }
        let root = node("/render-fixture", children: branches + (0..<4).map { node("/render-fixture/small-\($0)", size: 1) })
        let size = CGSize(width: 600, height: 600)
        let metrics = SunburstMetrics(size: size)
        let cache = SunburstPresentationCache()
        let mapRevision = UUID()
        let emptyCoverage = CollectionCoverage()
        let presentation = cache.presentation(root: root, mapRevision: mapRevision, coverage: emptyCoverage)
        let segments = presentation.layout.segments

        func point(_ segment: SunburstSegment, radialFraction: CGFloat, angularFraction: Double) -> CGPoint {
            let radius = metrics.innerRadius + CGFloat(segment.depth) * metrics.ringWidth + 1.4
                + max(1, metrics.ringWidth - 2.8) * radialFraction
            let angle = segment.start + (segment.end - segment.start) * angularFraction - .pi / 2
            return CGPoint(x: metrics.center.x + radius * cos(angle), y: metrics.center.y + radius * sin(angle))
        }
        let interiorPoints = segments.flatMap { segment in
            [0.2, 0.5, 0.8].flatMap { fraction in
                [0.25, 0.5, 0.75].map { radial in point(segment, radialFraction: radial, angularFraction: fraction) }
            }.filter { sample in
                // Keep antialiased edges and the independent hover outline outside the color sample.
                [CGPoint(x: -4, y: 0), CGPoint(x: 4, y: 0), CGPoint(x: 0, y: -4), CGPoint(x: 0, y: 4)]
                    .allSatisfy { segment.path(metrics: metrics).contains(CGPoint(x: sample.x + $0.x, y: sample.y + $0.y)) }
            }
        }
        let window = NSWindow(contentRect: CGRect(x: -20_000, y: -20_000, width: size.width, height: size.height),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let host = NSHostingView(rootView: AnyView(EmptyView()))
        window.contentView = host
        window.orderFront(nil)
        defer { window.close() }
        func render(hovered: String? = nil, selected: String? = nil,
                    coverage: CollectionCoverage = CollectionCoverage()) async throws -> Pixels {
            let map = SunburstView(root: root, mapRevision: mapRevision, cache: cache,
                                   selectedID: selected, hoveredID: hovered, collectionCoverage: coverage,
                                   onSelect: { _ in }, onOpen: { _ in }, onBack: {}, canGoBack: false,
                                   onHover: { _, _ in }, makeDrag: { _, _ in nil }, onDragEnded: { _ in })
                .frame(width: size.width, height: size.height)
                .background(Color(red: 0.04, green: 0.05, blue: 0.07))
                .environment(\.colorScheme, .dark)
            host.rootView = AnyView(map)
            host.setFrameSize(size)
            for _ in 0..<3 {
                host.layoutSubtreeIfNeeded()
                host.displayIfNeeded()
                try await Task.sleep(nanoseconds: 30_000_000)
            }
            guard let bitmap = host.bitmapImageRepForCachingDisplay(in: host.bounds) else {
                fatalError("AppKit did not create a render target")
            }
            host.cacheDisplay(in: host.bounds, to: bitmap)
            guard let image = bitmap.cgImage else { fatalError("AppKit did not render the map") }
            return Pixels(image: image, scale: CGFloat(image.width) / size.width)
        }
        func whiteOutlineSamples(_ image: Pixels, segment: SunburstSegment) -> Int {
            (1..<20).filter { index in
                [-0.5, 0, 0.5].contains { offset in
                    let radial = 1 + offset / max(1, metrics.ringWidth - 2.8)
                    let rgb = image.rgb(at: point(segment, radialFraction: radial, angularFraction: Double(index) / 20))
                    return rgb.min()! > 210
                }
            }.count
        }
        let baseline = try await render()
        check(interiorPoints.count >= 100, "fixture samples the interior of every substantial branch and depth")
        check(interiorPoints.allSatisfy { baseline.rgb(at: $0).max()! > 150 }, "the renderer produced colored graph pixels")
        let hoverSequence = [branches[0].id, nil, branches[1].id, branches[1].children[0].id,
                             nil, branches[2].id, "group:\(root.id)", nil]
        var maximumColorChange = 0
        for hovered in hoverSequence {
            let snapshot = try await render(hovered: hovered)
            let difference = interiorPoints.map { snapshot.difference(from: baseline, at: $0) }.max()!
            maximumColorChange = max(maximumColorChange, difference)
            check(difference <= 1, "branch interiors stay stable during hover/gap transition \(hovered ?? "none"): \(difference)/255")
        }
        let first = presentation.layout.segment(for: branches[0].id)!
        let second = presentation.layout.segment(for: branches[1].id)!
        let hovered = try await render(hovered: first.id)
        check(whiteOutlineSamples(hovered, segment: first) >= 15, "hover outline remains visible")
        check(whiteOutlineSamples(baseline, segment: first) == 0, "inactive branches have no hover outline")
        let selectedAndHovered = try await render(hovered: second.id, selected: first.id)
        check(whiteOutlineSamples(selectedAndHovered, segment: first) >= 15, "selection outline survives hover on another branch")
        check(whiteOutlineSamples(selectedAndHovered, segment: second) >= 15, "hover outline coexists with selection")

        let collectedCoverage = CollectionCoverage(paths: [first.id])
        let collected = try await render(coverage: collectedCoverage)
        let branchPoints = interiorPoints.filter { first.path(metrics: metrics).contains($0) }
        check(branchPoints.contains { collected.difference(from: baseline, at: $0) > 50 }, "collecting a branch changes its visible hatch or badge")
        let unaffected = interiorPoints.filter { sample in !first.path(metrics: metrics).contains(sample) &&
            !branches[0].children.contains { child in presentation.layout.segment(for: child.id)!.path(metrics: metrics).contains(sample) } }
        check(unaffected.allSatisfy { collected.difference(from: baseline, at: $0) <= 1 }, "collection marks leave unrelated branch colors unchanged")
        let collectedHovered = try await render(hovered: second.id, coverage: collectedCoverage)
        check(interiorPoints.allSatisfy { collectedHovered.difference(from: collected, at: $0) <= 1 }, "hover does not alter collected branch marks")
        let cleared = try await render()
        check(interiorPoints.allSatisfy { cleared.difference(from: baseline, at: $0) <= 1 }, "removing the collection restores the base graph")

        #if !PIXEL_ONLY
        let base = SunburstBaseLayer(presentation: cache.presentation(root: root, mapRevision: mapRevision, coverage: emptyCoverage), metrics: metrics)
        for id in hoverSequence {
            _ = try await render(hovered: id)
            let next = SunburstBaseLayer(presentation: cache.presentation(root: root, mapRevision: mapRevision, coverage: emptyCoverage), metrics: metrics)
            check(base == next, "hover leaves the cached base layer reusable: \(id ?? "none")")
        }
        let changedCollection = SunburstBaseLayer(presentation: cache.presentation(root: root, mapRevision: mapRevision, coverage: collectedCoverage), metrics: metrics)
        check(base != changedCollection, "collection changes invalidate the base layer")
        check(base.presentation.layout === changedCollection.presentation.layout, "collection changes preserve prepared geometry")
        check(base != SunburstBaseLayer(presentation: base.presentation, metrics: SunburstMetrics(size: CGSize(width: 500, height: 500))), "resize invalidates the base layer")
        let refreshed = SunburstBaseLayer(presentation: cache.presentation(root: root, mapRevision: UUID(), coverage: emptyCoverage), metrics: metrics)
        check(base != refreshed, "map refresh invalidates the base layer")
        #endif
        print("\(checks) rendering checks; \(interiorPoints.count) interior pixels per state; maximum hover color change \(maximumColorChange)/255; \(failures.count) failures")
        if !failures.isEmpty { exit(1) }
    }
}
