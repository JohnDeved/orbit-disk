import AppKit
import Foundation

// Renders the README feature cards from the captured app states, in light and dark variants.
// Each card shows one interaction as before → after, using a real captured state for each frame,
// annotated with the system pointer where you act and numbered steps placed next to their targets.
// Titles are README headings, so the app crops fill the card.
//   card-map-*.png      click the Projects arc on Home (overview) → Projects opens (projects-open);
//                       the center goes back up
//   card-collect-*.png  drag the selected Downloads arc (downloads) into the tray → it is marked on the
//                       map and list (collection)
//   card-review-*.png   open the review → Move to Trash or Delete Permanently, each confirmed
//   Cards INPUT_DIRECTORY OUTPUT_DIRECTORY
// Each card sits on a themed "spotlight" panel, picked by the README with <picture>: light is a soft
// peach-to-periwinkle gradient, dark is charcoal with a coral glow (top left) and an indigo glow
// (bottom right). Faint orbit rings echo the app icon. Corners outside the panel are transparent.
// Every capture is a full 1280 × 810 pt window; crops below are in window points. PNGs are
// written at 1.5× (1800 px: sharp in a Retina README column, lighter than 2×).

precondition(CommandLine.arguments.count == 3, "Usage: Cards INPUT_DIRECTORY OUTPUT_DIRECTORY")
_ = NSApplication.shared
NSApplication.shared.setActivationPolicy(.prohibited)
let input = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let output = URL(fileURLWithPath: CommandLine.arguments[2], isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)

/// The card being drawn; its height fits the card's content (see `render`).
var card = NSSize(width: 1200, height: 640)
let pixelScale: CGFloat = 1.5

func srgb(_ hex: UInt32, _ alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
            blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
}
/// A top-down rectangle in card points, converted to AppKit's bottom-left origin.
func box(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat) -> NSRect {
    NSRect(x: x, y: card.height - y - h, width: w, height: h)
}
func p(_ x: CGFloat, _ y: CGFloat) -> NSPoint { NSPoint(x: x, y: card.height - y) }

var captures: [String: CGImage] = [:]
func capture(_ name: String) -> CGImage {
    if let image = captures[name] { return image }
    guard let image = NSImage(contentsOf: input.appendingPathComponent(name)),
          let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { fatalError("Capture missing: \(name)") }
    captures[name] = cg
    return cg
}

struct Theme {
    let name: String
    /// Behind arrows and leaders where they cross map rings: close to the panel color.
    let halo: NSColor
    let pillFill: NSColor
    let pillText: NSColor
    /// A hairline around pills where the page is light.
    let pillEdge: NSColor?
    let pillShadow: CGFloat
    let accent: NSColor
    /// Opacity of the soft shadow under app crops.
    let shadow: CGFloat
}
let themes = [
    Theme(name: "dark", halo: srgb(0x262932, 0.85), pillFill: srgb(0xF2EEE6), pillText: srgb(0x17171A), pillEdge: nil,
          pillShadow: 0.45, accent: srgb(0xFF8F66), shadow: 0.6),
    Theme(name: "light", halo: srgb(0xFFFFFF, 0.9), pillFill: srgb(0xFFFFFF), pillText: srgb(0x1F242C),
          pillEdge: NSColor.black.withAlphaComponent(0.09), pillShadow: 0.16, accent: srgb(0xD4562D), shadow: 0.24),
]
let coral = srgb(0xFF8F66)
let ink = srgb(0x17171A)

/// Draws horizontal bands of one capture, stacked without gaps, as a single rounded panel with its
/// top-left corner at (x, y). Several bands cut empty space out of a tall sheet (e.g. an empty list)
/// while keeping its header and buttons. Returns the panel's size in card points.
@discardableResult
func picture(_ file: String, x crop: ClosedRange<CGFloat>, bands: [ClosedRange<CGFloat>], at x: CGFloat, _ y: CGFloat,
             scale: CGFloat = 1, radius: CGFloat = 12, shadow: CGFloat) -> NSSize {
    let cg = capture(file)
    let source = CGFloat(cg.width) / 1280
    let width = (crop.upperBound - crop.lowerBound) * scale
    let height = bands.reduce(0) { $0 + ($1.upperBound - $1.lowerBound) } * scale
    let rect = box(x, y, width, height)
    let path = NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius)
    NSGraphicsContext.saveGraphicsState()
    let drop = NSShadow()
    drop.shadowColor = NSColor.black.withAlphaComponent(shadow)
    drop.shadowBlurRadius = 30
    drop.shadowOffset = NSSize(width: 0, height: -10)
    drop.set()
    srgb(0x16181D).setFill()
    path.fill()
    NSGraphicsContext.restoreGraphicsState()
    NSGraphicsContext.saveGraphicsState()
    path.addClip()
    NSGraphicsContext.current?.imageInterpolation = .high
    var top = y
    for band in bands {
        let size = NSSize(width: crop.upperBound - crop.lowerBound, height: band.upperBound - band.lowerBound)
        guard let piece = cg.cropping(to: CGRect(x: crop.lowerBound * source, y: band.lowerBound * source,
                                                 width: size.width * source, height: size.height * source)) else { fatalError("Cannot crop \(file)") }
        NSImage(cgImage: piece, size: size).draw(in: box(x, top, size.width * scale, size.height * scale),
                                                 from: NSRect(origin: .zero, size: size), operation: .sourceOver, fraction: 1)
        top += size.height * scale
    }
    NSGraphicsContext.restoreGraphicsState()
    NSColor.white.withAlphaComponent(0.14).setStroke()
    let outline = NSBezierPath(roundedRect: rect.insetBy(dx: 0.5, dy: 0.5), xRadius: radius, yRadius: radius)
    outline.lineWidth = 1
    outline.stroke()
    return NSSize(width: width, height: height)
}

/// A rounded curve with an open arrowhead, in top-down card points, over a halo in the card's
/// background color so it stays visible across colored map rings. Dashed means a drag.
func arrow(_ start: CGPoint, _ c1: CGPoint, _ c2: CGPoint, _ end: CGPoint, theme: Theme, dashed: Bool = false) {
    let curve = NSBezierPath()
    curve.move(to: p(start.x, start.y))
    curve.curve(to: p(end.x, end.y), controlPoint1: p(c1.x, c1.y), controlPoint2: p(c2.x, c2.y))
    let dx = end.x - c2.x, dy = end.y - c2.y
    let length = hypot(dx, dy), ux = dx / length, uy = dy / length
    let head = NSBezierPath()
    head.move(to: p(end.x - 16 * ux - 9 * uy, end.y - 16 * uy + 9 * ux))
    head.line(to: p(end.x, end.y))
    head.line(to: p(end.x - 16 * ux + 9 * uy, end.y - 16 * uy - 9 * ux))
    for (width, stroke) in [(10.0, theme.halo), (4.0, theme.accent)] as [(CGFloat, NSColor)] {
        stroke.setStroke()
        for path in [curve, head] {
            path.lineWidth = width
            path.lineCapStyle = .round
            path.lineJoinStyle = .round
            let dash = dashed && path === curve && width < 10
            path.setLineDash(dash ? [11, 10] : [], count: dash ? 2 : 0, phase: 0)
            path.stroke()
        }
    }
}

/// A coral ring around a target (top-down card rect), marking where a step acts.
func target(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, radius: CGFloat, theme: Theme) {
    let path = NSBezierPath(roundedRect: box(x, y, w, h).insetBy(dx: -5, dy: -5), xRadius: radius + 5, yRadius: radius + 5)
    path.lineWidth = 3
    theme.accent.setStroke()
    path.stroke()
}

/// A thin coral leader from a step badge to its target.
func leader(from start: CGPoint, to end: CGPoint, theme: Theme) {
    let line = NSBezierPath()
    line.move(to: p(start.x, start.y))
    line.line(to: p(end.x, end.y))
    line.lineCapStyle = .round
    for (width, color) in [(7.0, theme.halo), (2.5, theme.accent)] as [(CGFloat, NSColor)] {
        line.lineWidth = width
        color.setStroke()
        line.stroke()
    }
}

/// The system pointer (pointing hand or closed hand), drawn 1.7× with its hot spot at (x, y).
func pointer(_ cursor: NSCursor, at x: CGFloat, _ y: CGFloat) {
    let scale: CGFloat = 1.7
    let size = cursor.image.size, hot = cursor.hotSpot
    let rect = box(x - hot.x * scale, y - hot.y * scale, size.width * scale, size.height * scale)
    NSGraphicsContext.saveGraphicsState()
    let drop = NSShadow()
    drop.shadowColor = NSColor.black.withAlphaComponent(0.45)
    drop.shadowBlurRadius = 6
    drop.shadowOffset = NSSize(width: 0, height: -2)
    drop.set()
    cursor.image.draw(in: rect)
    NSGraphicsContext.restoreGraphicsState()
}

/// A numbered step: a coral badge and a solid label pill, the badge's center at (x, y).
/// `leading` puts the pill to the left of the badge instead of the right.
func step(_ number: Int, _ label: String, at x: CGFloat, _ y: CGFloat, leading: Bool = false, theme: Theme) {
    let font = NSFont.systemFont(ofSize: 31, weight: .semibold)
    let attrs: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: theme.pillText]
    let size = (label as NSString).size(withAttributes: attrs)
    let badge: CGFloat = 54, height: CGFloat = 58, pad: CGFloat = 22
    let width = badge / 2 + 12 + size.width + pad
    let pillX = leading ? x - width : x
    let textX = leading ? x - badge / 2 - 12 - size.width : x + badge / 2 + 12
    NSGraphicsContext.saveGraphicsState()
    let drop = NSShadow()
    drop.shadowColor = NSColor.black.withAlphaComponent(theme.pillShadow)
    drop.shadowBlurRadius = 14
    drop.shadowOffset = NSSize(width: 0, height: -3)
    drop.set()
    theme.pillFill.setFill()
    let pill = NSBezierPath(roundedRect: box(pillX, y - height / 2, width, height), xRadius: height / 2, yRadius: height / 2)
    pill.fill()
    NSGraphicsContext.restoreGraphicsState()
    if let edge = theme.pillEdge {
        edge.setStroke()
        pill.lineWidth = 1
        pill.stroke()
    }
    (label as NSString).draw(at: NSPoint(x: textX, y: card.height - y - size.height / 2), withAttributes: attrs)
    coral.setFill()
    NSBezierPath(ovalIn: box(x - badge / 2, y - badge / 2, badge, badge)).fill()
    let numberAttrs: [NSAttributedString.Key: Any] = [.font: NSFont.systemFont(ofSize: 29, weight: .bold), .foregroundColor: ink]
    let digits = "\(number)" as NSString
    let numberSize = digits.size(withAttributes: numberAttrs)
    digits.draw(at: NSPoint(x: x - numberSize.width / 2, y: card.height - y - numberSize.height / 2), withAttributes: numberAttrs)
}

/// How far content moves down inside its panel (see `render`).
let contentShift: CGFloat = 19

/// Fills the card with the theme's spotlight panel.
func spotlight(theme: Theme) {
    let dark = theme.name == "dark"
    let shape = NSBezierPath(roundedRect: NSRect(origin: .zero, size: card).insetBy(dx: 1, dy: 1), xRadius: 28, yRadius: 28)
    NSGraphicsContext.saveGraphicsState()
    shape.addClip()
    if dark {
        srgb(0x262932).setFill()
        shape.fill()
        let coralCenter = NSPoint(x: card.width * 0.18, y: card.height * 0.85), indigoCenter = NSPoint(x: card.width * 0.85, y: card.height * 0.12)
        NSGradient(colors: [srgb(0xFF8F66, 0.05), srgb(0xFF8F66, 0)])!
            .draw(fromCenter: coralCenter, radius: 0, toCenter: coralCenter, radius: 620, options: [])
        NSGradient(colors: [srgb(0x6E6BFF, 0.05), srgb(0x6E6BFF, 0)])!
            .draw(fromCenter: indigoCenter, radius: 0, toCenter: indigoCenter, radius: 640, options: [])
    } else {
        NSGradient(colors: [srgb(0xFFEDE4), srgb(0xF6F1FA), srgb(0xE6ECFF)], atLocations: [0, 0.5, 1], colorSpace: .sRGB)!
            .draw(in: shape, angle: -25)
    }
    let center = NSPoint(x: card.width / 2, y: card.height / 2)
    for (radius, alpha) in zip([260, 400, 560] as [CGFloat], dark ? [0.09, 0.065, 0.045] : [0.10, 0.075, 0.05] as [CGFloat]) {
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

func render(_ name: String, height: CGFloat = 640, theme: Theme, _ content: () -> Void) throws {
    card = NSSize(width: 1200, height: height)
    guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(card.width * pixelScale), pixelsHigh: Int(card.height * pixelScale),
                                    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                                    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0) else { fatalError("Cannot allocate card") }
    rep.size = card
    guard let context = NSGraphicsContext(bitmapImageRep: rep) else { fatalError("Cannot draw card") }
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.cgContext.clear(CGRect(origin: .zero, size: card))

    spotlight(theme: theme)
    // Content is laid out from y = 0 with its first row of pills at y ≈ 21; shifting it down gives
    // the same 40 pt inset at the top as at the sides. Each card's height adds the same at the bottom.
    NSGraphicsContext.saveGraphicsState()
    let shift = NSAffineTransform()
    shift.translateX(by: 0, yBy: -contentShift)
    shift.concat()
    content()
    NSGraphicsContext.restoreGraphicsState()

    context.flushGraphics()
    NSGraphicsContext.restoreGraphicsState()
    guard let png = rep.representation(using: .png, properties: [:]) else { fatalError("PNG encoding failed") }
    try png.write(to: output.appendingPathComponent("\(name)-\(theme.name).png"), options: .atomic)
    print("Rendered \(name)-\(theme.name).png: \(rep.pixelsWide) × \(rep.pixelsHigh)")
}

// The map (rings and center) with a little room around its outer ring, without the size stats below
// it: 408 pt square from (359, 120).
let mapX: ClosedRange<CGFloat> = 359...767
let mapY: ClosedRange<CGFloat> = 120...528
/// A window point of a crop starting at (cropX, cropY), placed at (left, top) with `scale`.
func place(_ x: CGFloat, _ y: CGFloat, cropX: CGFloat, cropY: CGFloat, left: CGFloat, top: CGFloat, scale: CGFloat) -> CGPoint {
    CGPoint(x: left + (x - cropX) * scale, y: top + (y - cropY) * scale)
}

/// The width of a step pill with `label` (badge included), to place it from its right end.
func stepWidth(_ label: String) -> CGFloat {
    let size = (label as NSString).size(withAttributes: [.font: NSFont.systemFont(ofSize: 31, weight: .semibold)])
    return 54 / 2 + 12 + size.width + 22 + 54 / 2
}

for theme in themes {
    // Map: click the Projects arc on Home (before), Projects opens (after); the center goes back up.
    try render("card-map", height: 677, theme: theme) {
        let scale: CGFloat = 1.1, side = (mapX.upperBound - mapX.lowerBound) * 1.1
        let before = (left: CGFloat(40), top: CGFloat(96)), after = (left: CGFloat(1160) - side, top: CGFloat(96))
        picture("orbit-overview.png", x: mapX, bands: [mapY], at: before.left, before.top, scale: scale, shadow: theme.shadow)
        picture("orbit-projects-open.png", x: mapX, bands: [mapY], at: after.left, after.top, scale: scale, shadow: theme.shadow)
        let arc = place(655, 250, cropX: mapX.lowerBound, cropY: mapY.lowerBound, left: before.left, top: before.top, scale: scale)
        arrow(CGPoint(x: arc.x + 22, y: arc.y - 16), CGPoint(x: 540, y: 160), CGPoint(x: 600, y: 180), CGPoint(x: after.left - 12, y: 230), theme: theme)
        pointer(.pointingHand, at: arc.x, arc.y)
        let row: CGFloat = 50
        step(1, "Click an arc", at: before.left + 27, row, theme: theme)
        // The opened folder's breadcrumb (window 238…420, 62…90), shown above the opened map.
        let crumb = (scale: CGFloat(1.15), width: CGFloat(198 * 1.15), height: CGFloat(40 * 1.15))
        let crumbLeft = 1155 - crumb.width, crumbTop = row - crumb.height / 2
        picture("orbit-projects-open.png", x: 230...428, bands: [56...96], at: crumbLeft, crumbTop, scale: crumb.scale, radius: 10,
                shadow: theme.shadow + 0.1)
        target(crumbLeft, crumbTop, crumb.width, crumb.height, radius: 10, theme: theme)
        step(2, "It opens", at: crumbLeft - 18 - stepWidth("It opens") + 27, row, theme: theme)
        // The center's "Up one folder" (window 563, 398).
        let up = place(563, 398, cropX: mapX.lowerBound, cropY: mapY.lowerBound, left: after.left, top: after.top, scale: scale)
        target(up.x - 55, up.y - 15, 110, 30, radius: 15, theme: theme)
        let bottomRow = after.top + side + 44
        leader(from: CGPoint(x: after.left + 70, y: bottomRow - 26), to: CGPoint(x: up.x - 40, y: up.y + 20), theme: theme)
        step(3, "Center goes back up", at: after.left + 40, bottomRow, theme: theme)
    }
    // Collect: drag the selected Downloads arc (before) down into the tray, which sits under the map as
    // in the app; it is marked on the map (after).
    try render("card-collect", height: 631, theme: theme) {
        let scale: CGFloat = 0.88, side = (mapX.upperBound - mapX.lowerBound) * 0.88
        let before = (left: CGFloat(40), top: CGFloat(96)), after = (left: CGFloat(1160) - side, top: CGFloat(96))
        picture("orbit-downloads.png", x: mapX, bands: [mapY], at: before.left, before.top, scale: scale, shadow: theme.shadow)
        let hand = place(455, 300, cropX: mapX.lowerBound, cropY: mapY.lowerBound, left: before.left, top: before.top, scale: scale)
        step(1, "Drag an arc or a row", at: before.left + 27, 50, theme: theme)
        // The tray, below the map like the app's footer, now holds it.
        let tray = (left: before.left, top: CGFloat(500), scale: CGFloat(0.95), width: CGFloat(290 * 0.95), height: CGFloat(76 * 0.95))
        picture("orbit-collection.png", x: 232...522, bands: [716...792], at: tray.left, tray.top, scale: tray.scale, radius: 12,
                shadow: theme.shadow + 0.1)
        arrow(CGPoint(x: hand.x + 8, y: hand.y + 30), CGPoint(x: hand.x - 18, y: 380), CGPoint(x: hand.x + 12, y: 450),
              CGPoint(x: hand.x + 22, y: tray.top - 12), theme: theme, dashed: true)
        pointer(.closedHand, at: hand.x, hand.y)
        step(2, "Drop it in the tray", at: tray.left + tray.width + 28 + 27, tray.top + tray.height / 2, theme: theme)
        // After: the same map with the Downloads arcs striped and checked.
        picture("orbit-collection.png", x: mapX, bands: [mapY], at: after.left, after.top, scale: scale, shadow: theme.shadow)
        arrow(CGPoint(x: tray.left + tray.width + 8, y: tray.top - 6), CGPoint(x: 430, y: 420), CGPoint(x: 640, y: 340),
              CGPoint(x: after.left - 12, y: 330), theme: theme)
        let hatch = place(436, 300, cropX: mapX.lowerBound, cropY: mapY.lowerBound, left: after.left, top: after.top, scale: scale)
        target(hatch.x - 30, hatch.y - 42, 66, 86, radius: 12, theme: theme)
        // Label 3 sits under the map on the same row as label 2, so the steps read left to right, with a
        // leader up to the circled stripes.
        let label = "Marked on the map", labelRow = tray.top + tray.height / 2
        let labelX = 1160 - stepWidth(label) + 27
        leader(from: CGPoint(x: hatch.x + 3, y: labelRow - 29), to: CGPoint(x: hatch.x + 3, y: hatch.y + 49), theme: theme)
        step(3, label, at: labelX, labelRow, theme: theme)
    }
    // Review: open the review from the tray, then choose: Move to Trash, or delete permanently.
    try render("card-review", height: 647, theme: theme) {
        let scale: CGFloat = 0.8
        // The tray's Review Collection button (window 1092…1240, 744…772).
        picture("orbit-collection.png", x: 1080...1252, bands: [736...780], at: 40, 124, scale: 1.1, radius: 12, shadow: theme.shadow)
        pointer(.pointingHand, at: 170, 160)
        step(1, "Open the review", at: 67, 50, theme: theme)
        // The review sheet without its empty list space: header and row, then the actions.
        let left: CGFloat = 400, top: CGFloat = 30
        picture("orbit-review.png", x: 281...1000, bands: [171...365, 545...639], at: left, top, scale: scale,
                radius: 14, shadow: theme.shadow)
        arrow(CGPoint(x: 248, y: 150), CGPoint(x: 300, y: 150), CGPoint(x: 340, y: 130), CGPoint(x: left - 12, y: 112), theme: theme)
        // Button rects in the review sheet (window): Delete Permanently… 682…838, Move to Trash… 849…973, y 583…613.
        let buttonTop = top + (194 + (583 - 545)) * scale, buttonBottom = buttonTop + 30 * scale
        let trashX = left + (849 - 281) * scale, deleteX = left + (682 - 281) * scale
        target(trashX, buttonTop, 124 * scale, 30 * scale, radius: 12, theme: theme)
        target(deleteX, buttonTop, 156 * scale, 30 * scale, radius: 12, theme: theme)
        let trashCenter = trashX + 62 * scale, deleteCenter = deleteX + 78 * scale
        // Label 2 hangs below Move to Trash…, right-aligned, clear of everything else.
        let trashLabel = "Trash: restore from Finder", trashRow = buttonBottom + 112
        leader(from: CGPoint(x: trashCenter, y: trashRow - 29), to: CGPoint(x: trashCenter, y: buttonBottom + 7), theme: theme)
        step(2, trashLabel, at: 1160 - stepWidth(trashLabel) + 27, trashRow, theme: theme)
        // Or Delete Permanently…: its confirmation, at the same scale, lists every path (empty list space cut).
        let dialog = (left: CGFloat(40), top: CGFloat(330))
        picture("orbit-permanent-delete.png", x: 321...960, bands: [191...425, 530...619], at: dialog.left, dialog.top, scale: scale,
                radius: 14, shadow: theme.shadow + 0.15)
        arrow(CGPoint(x: deleteCenter - 20, y: buttonBottom + 8), CGPoint(x: deleteCenter - 60, y: 300), CGPoint(x: 600, y: 300),
              CGPoint(x: dialog.left + 470, y: dialog.top - 10), theme: theme)
        // Its Delete Permanently button (window 787…933, 563…593), below the cut list space.
        let confirmX = dialog.left + (787 - 321) * scale, confirmTop = dialog.top + (234 + (563 - 530)) * scale
        target(confirmX, confirmTop, 146 * scale, 30 * scale, radius: 12, theme: theme)
        let deleteLabel = "Or delete permanently", deleteRow = confirmTop + 12 * scale
        leader(from: CGPoint(x: dialog.left + 639 * scale + 30, y: deleteRow), to: CGPoint(x: confirmX + 146 * scale + 7, y: deleteRow), theme: theme)
        step(3, deleteLabel, at: dialog.left + 639 * scale + 30 + 27, deleteRow, theme: theme)
    }
}
