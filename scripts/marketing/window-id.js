// Prints the CGWindowID of the largest on-screen, normal-layer window owned by a process.
// Usage: osascript -l JavaScript scripts/marketing/window-id.js PID
// Window numbers and owners are readable without Screen Recording permission (titles are not).
ObjC.import('CoreGraphics');

function run(argv) {
  const pid = Number(argv[0]);
  const options = $.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements;
  const list = ObjC.castRefToObject($.CGWindowListCopyWindowInfo(options, $.kCGNullWindowID));
  const windows = (ObjC.deepUnwrap(list) || [])
    .filter((w) => w.kCGWindowOwnerPID === pid && w.kCGWindowLayer === 0)
    .sort((a, b) => b.kCGWindowBounds.Width * b.kCGWindowBounds.Height - a.kCGWindowBounds.Width * a.kCGWindowBounds.Height);
  return windows.length ? `${windows[0].kCGWindowNumber} ${windows[0].kCGWindowBounds.Width}x${windows[0].kCGWindowBounds.Height}` : '';
}
