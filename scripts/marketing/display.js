// Prints the screens and the main display's modes. With WIDTH HEIGHT (window content size in
// points), switches the main display to the smallest mode that fits that window, preferring
// Retina (2x) modes, so screenshots are taken at full size. CI runners boot into a small 1x mode.
//
//   osascript -l JavaScript scripts/marketing/display.js [WIDTH HEIGHT]
ObjC.import('AppKit');
ObjC.import('CoreGraphics');

// Room for the menu bar and the window's title bar above the content.
const CHROME = 80;

function screens() {
  const list = $.NSScreen.screens;
  const out = [];
  for (let i = 0; i < list.count; i++) {
    const screen = list.objectAtIndex(i);
    const frame = screen.frame;
    const visible = screen.visibleFrame;
    out.push(`screen ${frame.size.width}x${frame.size.height} @${screen.backingScaleFactor}x, ` +
      `visible ${visible.size.width}x${visible.size.height}`);
  }
  return out;
}

function modes(display) {
  const options = $.NSDictionary.dictionaryWithObjectForKey(
    $.NSNumber.numberWithBool(true), 'kCGDisplayShowDuplicateLowResolutionModes');
  const all = ObjC.castRefToObject($.CGDisplayCopyAllDisplayModes(display, options));
  const out = [];
  for (let i = 0; i < all.count; i++) {
    const mode = all.objectAtIndex(i);
    const width = $.CGDisplayModeGetWidth(mode);
    out.push({ mode, width, height: $.CGDisplayModeGetHeight(mode), scale: $.CGDisplayModeGetPixelWidth(mode) / width });
  }
  return out;
}

function run(argv) {
  const display = $.CGMainDisplayID();
  const available = modes(display);
  const lines = screens();
  const names = [...new Set(available.map(m => `${m.width}x${m.height}@${m.scale}x`))];
  lines.push(`modes: ${names.join(' ')}`);
  if (argv.length < 2) return lines.join('\n');
  const [width, height] = argv.map(Number);
  const fits = available
    .filter(m => m.width >= width && m.height >= height + CHROME)
    .sort((a, b) => (b.scale - a.scale) || (a.width * a.height - b.width * b.height));
  if (fits.length === 0) {
    lines.push(`no mode fits a ${width}x${height} window; leaving the display as it is`);
    return lines.join('\n');
  }
  const best = fits[0];
  const error = $.CGDisplaySetDisplayMode(display, best.mode, $());
  lines.push(error === 0
    ? `switched to ${best.width}x${best.height}@${best.scale}x`
    : `couldn't switch to ${best.width}x${best.height}@${best.scale}x (error ${error})`);
  delay(2);
  return lines.concat(screens()).join('\n');
}
