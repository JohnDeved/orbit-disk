import AppKit
import Foundation

let input = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let output = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)

func render(_ name: String, height: CGFloat, _ block: (CGFloat) -> Void) throws {
    let size = NSSize(width: 1360, height: height)
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size.width * 2), pixelsHigh: Int(size.height * 2),
                                    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { fatalError("Cannot allocate flow image") }
    rep.size = size
    guard let context = NSGraphicsContext(bitmapImageRep: rep) else { fatalError("Cannot draw flow image") }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.clear(CGRect(origin: .zero, size: size))
    block(height)
    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()
    guard let png = rep.representation(using: .png, properties: [:]) else { fatalError("PNG encoding failed") }
    try png.write(to: output.appendingPathComponent(name))
}
func box(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ height: CGFloat) -> NSRect {
    NSRect(x: x, y: height - y - h, width: w, height: h)
}
func text(_ string: String, _ x: CGFloat, _ y: CGFloat, _ height: CGFloat, size: CGFloat, weight: NSFont.Weight, color: NSColor) {
    let font = NSFont.systemFont(ofSize: size, weight: weight)
    let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color]
    let measured = (string as NSString).size(withAttributes: attrs)
    (string as NSString).draw(at: NSPoint(x: x, y: height - y - measured.height), withAttributes: attrs)
}
func picture(_ file: String, crop: CGRect, into rect: NSRect) {
    guard let src = NSImage(contentsOf: input.appendingPathComponent(file)),
          let cg = src.cgImage(forProposedRect: nil, context: nil, hints: nil) else { fatalError("Could not read \(file)") }
    let pixelScale = file == "orbit-review.png" ? 1 : CGFloat(cg.width) / 1280
    let pixelCrop = CGRect(x: crop.minX * pixelScale, y: crop.minY * pixelScale,
                           width: crop.width * pixelScale, height: crop.height * pixelScale)
    guard let cropped = cg.cropping(to: pixelCrop) else { fatalError("Could not crop \(file)") }
    NSGraphicsContext.saveGraphicsState()
    NSBezierPath(roundedRect: rect, xRadius: 10, yRadius: 10).addClip()
    NSImage(cgImage: cropped, size: crop.size).draw(in: rect, from: NSRect(origin: .zero, size: crop.size), operation: .sourceOver, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    NSColor.white.withAlphaComponent(0.13).setStroke()
    let outline = NSBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: 10, yRadius: 10)
    outline.lineWidth = 1
    outline.stroke()
}
func arrow(_ start: CGPoint, _ c1: CGPoint, _ c2: CGPoint, _ end: CGPoint, _ height: CGFloat, color: NSColor) {
    func p(_ v: CGPoint) -> NSPoint { NSPoint(x: v.x, y: height - v.y) }
    let curve = NSBezierPath()
    curve.move(to: p(start))
    curve.curve(to: p(end), controlPoint1: p(c1), controlPoint2: p(c2))
    curve.lineWidth = 3
    curve.lineCapStyle = .round
    color.setStroke(); curve.stroke()
    let dx = end.x - c2.x, dy = end.y - c2.y
    let length = hypot(dx, dy), ux = dx / length, uy = dy / length
    let tip = p(end)
    let left = p(CGPoint(x: end.x - 13 * ux - 6 * uy, y: end.y - 13 * uy + 6 * ux))
    let right = p(CGPoint(x: end.x - 13 * ux + 6 * uy, y: end.y - 13 * uy - 6 * ux))
    let head = NSBezierPath(); head.move(to: left); head.line(to: tip); head.line(to: right)
    head.lineWidth = 3; head.lineCapStyle = .round; head.lineJoinStyle = .round; head.stroke()
}
for isDark in [false, true] {
    let theme = isDark ? "dark" : "light"
    let paper = isDark ? NSColor(srgbRed: 0.90, green: 0.93, blue: 0.95, alpha: 1)
                       : NSColor(srgbRed: 0.14, green: 0.16, blue: 0.18, alpha: 1)
    let muted = isDark ? NSColor(srgbRed: 0.69, green: 0.73, blue: 0.77, alpha: 1)
                       : NSColor(srgbRed: 0.34, green: 0.38, blue: 0.42, alpha: 1)
    let coral = isDark ? NSColor(srgbRed: 1.0, green: 0.55, blue: 0.40, alpha: 1)
                   : NSColor(srgbRed: 0.83, green: 0.337, blue: 0.176, alpha: 1)
    try render("orbit-navigate-\(theme).png", height: 1650) { h in
        text("Map and list show the same folder", 75, 48, h, size: 23, weight: .semibold, color: paper)
        text("HOME", 1190, 53, h, size: 15, weight: .semibold, color: muted)
        picture("orbit-overview.png", crop: CGRect(x: 210, y: 100, width: 1070, height: 520), into: box(75, 100, 1210, 588, h))
        arrow(CGPoint(x: 690, y: 288), CGPoint(x: 735, y: 250), CGPoint(x: 790, y: 248), CGPoint(x: 832, y: 270), h, color: coral)
        text("Open Projects, then select Studio", 75, 800, h, size: 23, weight: .semibold, color: paper)
        text("PROJECTS", 1165, 805, h, size: 15, weight: .semibold, color: muted)
        arrow(CGPoint(x: 1215, y: 260), CGPoint(x: 1345, y: 240), CGPoint(x: 1360, y: 740), CGPoint(x: 1287, y: 829), h, color: coral)
        picture("orbit-projects.png", crop: CGRect(x: 210, y: 0, width: 1070, height: 620), into: box(75, 850, 1210, 701, h))
        text("Use the center or Up to return", 75, 1580, h, size: 18, weight: .regular, color: muted)
    }
    try render("orbit-collect-\(theme).png", height: 1840) { h in
        text("Drag a segment or row into the collection", 75, 48, h, size: 23, weight: .semibold, color: paper)
        picture("orbit-collection.png", crop: CGRect(x: 210, y: 85, width: 1070, height: 725), into: box(75, 100, 1210, 820, h))
        arrow(CGPoint(x: 330, y: 385), CGPoint(x: 200, y: 520), CGPoint(x: 240, y: 790), CGPoint(x: 600, y: 850), h, color: coral)
        text("Review before removing", 75, 1000, h, size: 23, weight: .semibold, color: paper)
        arrow(CGPoint(x: 1190, y: 870), CGPoint(x: 1355, y: 870), CGPoint(x: 1355, y: 1030), CGPoint(x: 1205, y: 1080), h, color: coral)
        // The review view is captured from the app; its pixel dimensions depend on the display scale.
        let url = input.appendingPathComponent("orbit-review.png")
        guard let image = NSImage(contentsOf: url), let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { fatalError("Missing review capture") }
        let width = CGFloat(cg.width), height = CGFloat(cg.height)
        let scale = 1040 / width
        let w = width * scale, hh = height * scale
        picture("orbit-review.png", crop: CGRect(x: 0, y: 0, width: width, height: height), into: box((1360-w)/2, 1100, w, hh, h))
    }

}
