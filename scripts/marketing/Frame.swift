import AppKit
import Foundation

precondition(CommandLine.arguments.count == 3, "Usage: Frame INPUT_DIRECTORY OUTPUT_DIRECTORY")
_ = NSApplication.shared
NSApplication.shared.setActivationPolicy(.prohibited)
let input = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let output = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
let names = ["orbit-overview.png", "orbit-collection.png"]
for filename in names {
    // The collection image omits the repeated sidebar to give the map and list more room.
    let crop = filename == "orbit-collection.png"
        ? NSRect(x: 210, y: 0, width: 1070, height: 810)
        : NSRect(x: 0, y: 0, width: 1280, height: 810)
    let canvas = NSSize(width: crop.width + 128, height: 946)
    let screenshotRect = NSRect(x: 64, y: 62, width: crop.width, height: crop.height)
    guard let source = NSImage(contentsOf: input.appendingPathComponent(filename)),
          let cg = source.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
        fatalError("Screenshot missing: \(filename)")
    }
    let pixelScale = CGFloat(cg.width) / 1280
    let pixelCrop = CGRect(x: crop.minX * pixelScale, y: crop.minY * pixelScale,
                           width: crop.width * pixelScale, height: crop.height * pixelScale)
    guard let cropped = cg.cropping(to: pixelCrop) else { fatalError("Cannot crop screenshot: \(filename)") }
    let screenshot = NSImage(cgImage: cropped, size: crop.size)
    guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(canvas.width * 2), pixelsHigh: Int(canvas.height * 2),
                                       bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                       colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { fatalError("Cannot allocate framed screenshot") }
    bitmap.size = canvas
    guard let context = NSGraphicsContext(bitmapImageRep: bitmap) else { fatalError("Cannot draw framed screenshot") }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.clear(CGRect(origin: .zero, size: canvas))
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
    screenshot.draw(in: screenshotRect, from: NSRect(origin: .zero, size: crop.size), operation: .sourceOver, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    NSColor.white.withAlphaComponent(0.12).setStroke()
    let outline = NSBezierPath(roundedRect: screenshotRect.insetBy(dx: 0.5, dy: 0.5), xRadius: 12, yRadius: 12)
    outline.lineWidth = 1
    outline.stroke()
    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()
    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        fatalError("Cannot encode framed screenshot: \(filename)")
    }
    try png.write(to: output.appendingPathComponent(filename), options: .atomic)
    print("Framed \(filename): \(bitmap.pixelsWide) × \(bitmap.pixelsHigh)")
}
