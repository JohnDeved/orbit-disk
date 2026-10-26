#!/usr/bin/env python3
"""Check that packaging rejects changed executable permissions."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

app = Path(sys.argv[1]).resolve(strict=True)
with tempfile.TemporaryDirectory() as temporary:
    folder = Path(temporary)
    wrapper = folder / "ditto"
    wrapper.write_text('#!/bin/sh\n/usr/bin/ditto "$@" || exit $?\n/bin/chmod a-x "$2/Contents/MacOS/OrbitDisk"\n')
    wrapper.chmod(0o755)
    image = folder / "changed.dmg"
    environment = dict(os.environ, PATH=f"{folder}:{os.environ['PATH']}")
    result = subprocess.run(
        ["./scripts/package-dmg.sh", str(app), str(image)],
        env=environment, capture_output=True, text=True,
    )
    if (result.returncode == 0 or image.exists()
            or "Disk image differs from the original app" not in result.stderr
            or "Contents/MacOS/OrbitDisk" not in result.stderr):
        raise SystemExit(f"Permission change was not rejected as expected:\n{result.stdout}{result.stderr}")
    print("Disk image with changed executable permissions was rejected.")
