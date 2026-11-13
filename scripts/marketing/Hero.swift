import AppKit
import Foundation

// Renders the README hero in light and dark variants (the README picks one with <picture>): the app
// window on the same "spotlight" panel as the step cards. Light is a soft peach-to-periwinkle
// gradient; dark is charcoal with a coral glow (top left) and an indigo glow (bottom right). Faint
// orbit rings echo the app icon. Margins stay narrow so the interface is as large as possible.
// PNGs are written at 1.25× (sharp in a Retina README column, about 60 % lighter than 2×).
//   Hero INPUT_DIRECTORY OUTPUT_DIRECTORY   (reads orbit-hero-state.png, writes orbit-hero-{light,dark}.png)
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
let margin = (side: CGFloat(56), top: CGFloat(48), bottom: CGFloat(60))
let pixelScale: CGFloat = 1.25
let canvas = NSSize(width: window.width + margin.side * 2, height: window.height + margin.top + margin.bottom)

guard let source = NSImage(contentsOf: input.appendingPathComponent("orbit-hero-state.png")),
      let cg = source.cgImage(forProposedRect: nil, context: nil, hints: nil) else {
    fatalError("Screenshot missing: orbit-hero-state.png")
}
let screenshot = NSImage(cgImage: cg, size: window)

/// The spotlight panel filling the canvas (same recipe as the step cards in Cards.swift).
func spotlight(dark: Bool) {
    let shape = NSBezierPath(roundedRect: NSRect(origin: .zero, size: canvas).insetBy(dx: 1, dy: 1), xRadius: 32, yRadius: 32)
    NSGraphicsContext.saveGraphicsState()
    shape.addClip()
    if dark {
        srgb(0x121318).setFill()
        shape.fill()
        let coralCenter = NSPoint(x: canvas.width * 0.18, y: canvas.height * 0.85)
        let indigoCenter = NSPoint(x: canvas.width * 0.85, y: canvas.height * 0.12)
        NSGradient(colors: [srgb(0xFF8F66, 0.30), srgb(0xFF8F66, 0)])!
            .draw(fromCenter: coralCenter, radius: 0, toCenter: coralCenter, radius: 820, options: [])
        NSGradient(colors: [srgb(0x6E6BFF, 0.26), srgb(0x6E6BFF, 0)])!
            .draw(fromCenter: indigoCenter, radius: 0, toCenter: indigoCenter, radius: 840, options: [])
    } else {
        NSGradient(colors: [srgb(0xFFEDE4), srgb(0xF6F1FA), srgb(0xE6ECFF)], atLocations: [0, 0.5, 1], colorSpace: .sRGB)!
            .draw(in: shape, angle: -25)
    }
    let center = NSPoint(x: canvas.width / 2, y: canvas.height / 2)
    for (radius, alpha) in zip([360, 540, 740] as [CGFloat], dark ? [0.09, 0.065, 0.045] : [0.10, 0.075, 0.05] as [CGFloat]) {
        let ring = NSBezierPath(ovalIn: NSRect(x: center.x - radius, y: center.y - radius, width: radius * 2, height: radius * 2))
        ring.lineWidth = 1.5
        (dark ? srgb(0xFFF5DE, alpha) : srgb(0x3B4560, alpha)).setStroke()
        ring.stroke()
    }
    NSGraphicsContext.restoreGraphicsState()
    (dark ? NSColor.white.withAlphaComponent(0.10) : NSColor.black.withAlphaComponent(0.06)).setStroke()
    shape.lineWidth = 1
    shape.stroke()
}

for dark in [false, true] {
    guard let bitmap = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(canvas.width * pixelScale), pixelsHigh: Int(canvas.height * pixelScale),
                                       bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                       colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { fatalError("Cannot allocate hero") }
    bitmap.size = canvas
    guard let context = NSGraphicsContext(bitmapImageRep: bitmap) else { fatalError("Cannot draw hero") }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.clear(CGRect(origin: .zero, size: canvas))
    spotlight(dark: dark)

    // The window (AppKit's origin is bottom-left), with a soft neutral shadow and a hairline edge.
    let windowRect = NSRect(x: margin.side, y: margin.bottom, width: window.width, height: window.height)
    let shadow = NSShadow()
    shadow.shadowColor = NSColor.black.withAlphaComponent(dark ? 0.5 : 0.28)
    shadow.shadowBlurRadius = 34
    shadow.shadowOffset = NSSize(width: 0, height: -12)
    NSGraphicsContext.saveGraphicsState()
    shadow.set()
    NSColor.black.setFill()
    NSBezierPath(roundedRect: windowRect, xRadius: 12, yRadius: 12).fill()
    NSGraphicsContext.restoreGraphicsState()
    NSGraphicsContext.saveGraphicsState()
    NSBezierPath(roundedRect: windowRect, xRadius: 12, yRadius: 12).addClip()
    screenshot.draw(in: windowRect, from: NSRect(origin: .zero, size: window), operation: .sourceOver, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    NSColor.white.withAlphaComponent(0.18).setStroke()
    let outline = NSBezierPath(roundedRect: windowRect.insetBy(dx: 0.5, dy: 0.5), xRadius: 12, yRadius: 12)
    outline.lineWidth = 1
    outline.stroke()

    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()
    guard let png = bitmap.representation(using: .png, properties: [:]) else { fatalError("Cannot encode hero") }
    let name = "orbit-hero-\(dark ? "dark" : "light").png"
    try png.write(to: output.appendingPathComponent(name), options: .atomic)
    print("Rendered \(name): \(bitmap.pixelsWide) × \(bitmap.pixelsHigh)")
}
