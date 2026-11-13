import AppKit
import Foundation

// Renders the README hero: the app window floating on a transparent canvas with a soft shadow, so it
// sits on GitHub's light and dark pages alike. Margins are just wide enough for the shadow to fade
// out, so the interface is as large as possible at README width.
// The PNG is written at 1.25× (sharp in a Retina README column, about 60 % lighter than 2×).
//   Hero INPUT_DIRECTORY OUTPUT_DIRECTORY   (reads orbit-hero-state.png, writes orbit-hero.png)
// The hero state shows the app in use: the largest folder selected, and a collected folder striped
// on the map, marked in the list and waiting in the tray next to an active Review Collection button.

precondition(CommandLine.arguments.count == 3, "Usage: Hero INPUT_DIRECTORY OUTPUT_DIRECTORY")
_ = NSApplication.shared
NSApplication.shared.setActivationPolicy(.prohibited)
let input = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let output = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

func srgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}

let window = NSSize(width: 1280, height: 810)
// Side and top margins hold the shadow; the bottom one also holds its downward offset.
let margin = (side: CGFloat(56), top: CGFloat(30), bottom: CGFloat(50))
let pixelScale: CGFloat = 1.25
let canvas = NSSize(width: window.width + margin.side * 2, height: window.height + margin.top + margin.bottom)

guard let source = NSImage(contentsOf: input.appendingPathComponent("orbit-hero-state.png")),
      let cg = source.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    fatalError("Screenshot missing: orbit-hero-state.png")
}
let screenshot = NSImage(cgImage: cg, size: window)
guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(canvas.width * pixelScale), pixelsHigh: Int(canvas.height * pixelScale),
                                   bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                   colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { fatalError("Cannot allocate hero") }
bitmap.size = canvas
guard let context = NSGraphicsContext(bitmapImageRep: bitmap) else { fatalError("Cannot draw hero") }
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = context
context.cgContext.clear(CGRect(origin: .zero, size: canvas))

// AppKit's origin is bottom-left; the layout below is in top-down points.
let windowRect = NSRect(x: margin.side, y: margin.bottom, width: window.width, height: window.height)

// No backdrop: the image is transparent around the window, so GitHub's page color (light, dark or
// dimmed) shows through. A neutral shadow fades out inside the margins, so the image has no
// visible edge; the hairline keeps the window defined on dark pages.
// The window, with a deep soft shadow and a hairline edge.
let shadow = NSShadow()
shadow.shadowColor = NSColor.black.withAlphaComponent(0.4)
shadow.shadowBlurRadius = 30
shadow.shadowOffset = NSSize(width: 0, height: -10)
NSGraphicsContext.saveGraphicsState()
shadow.set()
NSColor.black.setFill()
NSBezierPath(roundedRect: windowRect, xRadius: 12, yRadius: 12).fill()
NSGraphicsContext.restoreGraphicsState()
NSGraphicsContext.saveGraphicsState()
NSBezierPath(roundedRect: windowRect, xRadius: 12, yRadius: 12).addClip()
screenshot.draw(in: windowRect, from: NSRect(origin: .zero, size: window), operation: .sourceOver, fraction: 1)
NSGraphicsContext.restoreGraphicsState()
NSColor.white.withAlphaComponent(0.22).setStroke()
let outline = NSBezierPath(roundedRect: windowRect.insetBy(dx: 0.5, dy: 0.5), xRadius: 12, yRadius: 12)
outline.lineWidth = 1
outline.stroke()

context.flushGraphics()
NSGraphicsContext.restoreGraphicsState()
guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("Cannot encode hero") }
try png.write(to: output.appendingPathComponent("orbit-hero.png"), options: .atomic)
print("Rendered orbit-hero.png: \(bitmap.pixelsWide) × \(bitmap.pixelsHigh)")
