import AppKit
import Foundation

precondition(CommandLine.arguments.count == 3, "Usage: Frame INPUT_DIRECTORY OUTPUT_DIRECTORY")
_ = NSApplication.shared
NSApplication.shared.setActivationPolicy(.prohibited)
let input = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let output = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
let names = [("orbit-overview.png", "EXPLORE"), ("orbit-collection.png", "COLLECT")]
for (filename, label) in names {
    // The collection image omits the repeated sidebar to give the map and list more room.
    let crop = filename == "orbit-collection.png"
        ? NSRect(x: 210, y: 0, width: 1070, height: 810)
        : NSRect(x: 0, y: 0, width: 1280, height: 810)
    let canvas = NSSize(width: crop.width + 128, height: 946)
    let screenshotRect = NSRect(x: 64, y: 62, width: crop.width, height: crop.height)
    guard let screenshot = NSImage(contentsOf: input.appendingPathComponent(filename)) else {
        fatalError("Screenshot missing: \(filename)")
    }
    let image = NSImage(size: canvas)
    image.lockFocus()
    let bounds = NSRect(origin: .zero, size: canvas)
    let gradient = NSGradient(starting: NSColor(calibratedRed: 0.105, green: 0.125, blue: 0.165, alpha: 1),
                              ending: NSColor(calibratedRed: 0.065, green: 0.075, blue: 0.10, alpha: 1))!
    gradient.draw(in: bounds, angle: -35)
    NSColor(calibratedRed: 1, green: 0.60, blue: 0.47, alpha: 0.16).setStroke()
    for inset in [CGFloat(0), 14, 28] {
        let orbit = NSBezierPath(ovalIn: NSRect(x: -205 + inset, y: -190 + inset,
                                                width: 410 - 2 * inset, height: 410 - 2 * inset))
        orbit.lineWidth = 1
        orbit.stroke()
    }
    let heading: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 13, weight: .semibold),
        .foregroundColor: NSColor(calibratedWhite: 0.88, alpha: 1),
        .kern: 1.5
    ]
    let subtitle: [NSAttributedString.Key: Any] = [
        .font: NSFont.systemFont(ofSize: 11, weight: .medium),
        .foregroundColor: NSColor(calibratedWhite: 0.62, alpha: 1),
        .kern: 1.2
    ]
    ("ORBIT DISK  /  \(label)" as NSString).draw(at: NSPoint(x: 64, y: 900), withAttributes: heading)
    ("SAMPLE DATA" as NSString).draw(at: NSPoint(x: canvas.width - 161, y: 901), withAttributes: subtitle)
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(0.46)
    shadow.shadowBlurRadius = 30
    shadow.shadowOffset = NSSize(width: 0, height: -12)
    NSGraphicsContext.saveGraphicsState()
    shadow.set()
    NSColor.black.setFill()
    NSBezierPath(roundedRect: screenshotRect, xRadius: 12, yRadius: 12).fill()
    NSGraphicsContext.restoreGraphicsState()
    NSGraphicsContext.saveGraphicsState()
    NSBezierPath(roundedRect: screenshotRect, xRadius: 12, yRadius: 12).addClip()
    screenshot.draw(in: screenshotRect, from: crop, operation: .sourceOver, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    NSColor.white.withAlphaComponent(0.12).setStroke()
    let outline = NSBezierPath(roundedRect: screenshotRect.insetBy(dx: 0.5, dy: 0.5), xRadius: 12, yRadius: 12)
    outline.lineWidth = 1
    outline.stroke()
    image.unlockFocus()
    guard let tiff = image.tiffRepresentation, let bitmap = NSBitmapImageRep(data: tiff),
          let png = bitmap.representation(using: .png, properties: [:]) else {
        fatalError("Cannot encode framed screenshot: \(filename)")
    }
    try png.write(to: output.appendingPathComponent(filename), options: .atomic)
    print("Framed \(filename): \(bitmap.pixelsWide) × \(bitmap.pixelsHigh)")
}
