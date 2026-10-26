#!/usr/bin/env python3
"""Apply Orbit Disk's English copy to the staged framework before signing."""

import json
import plistlib
from pathlib import Path
import sys


def customize(framework: Path) -> None:
    strings = framework / "Versions/Current/Resources/Base.lproj/Sparkle.strings"
    with strings.open("rb") as source:
        original = plistlib.load(source)
    overlay_path = Path(__file__).resolve().parent.parent / "Assets/Sparkle.en.json"
    overlay = json.loads(overlay_path.read_text())
    missing = overlay.keys() - original.keys()
    if missing:
        raise ValueError(f"Sparkle copy changed; review the English overlay. Missing keys: {sorted(missing)}")
    original.update(overlay)
    strings.write_bytes(plistlib.dumps(original, fmt=plistlib.FMT_BINARY))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("Usage: customize-sparkle.py PATH/TO/STAGED/Sparkle.framework")
    customize(Path(sys.argv[1]))
