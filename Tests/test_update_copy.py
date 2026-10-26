import importlib.util
import json
import plistlib
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("customize_sparkle", ROOT / "scripts/customize-sparkle.py")
copy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(copy)


class UpdateCopyTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.framework = Path(self.directory.name) / "Sparkle.framework"
        self.strings = self.framework / "Versions/Current/Resources/Base.lproj/Sparkle.strings"
        self.strings.parent.mkdir(parents=True)
        vendor = ROOT / ".build/artifacts/sparkle/Sparkle/Sparkle.xcframework/macos-arm64_x86_64/Sparkle.framework/Versions/Current/Resources/Base.lproj/Sparkle.strings"
        self.original = plistlib.loads(vendor.read_bytes())
        self.strings.write_bytes(plistlib.dumps(self.original, fmt=plistlib.FMT_BINARY))

    def test_overlay_preserves_other_copy_and_action_meanings(self):
        copy.customize(self.framework)
        actual = plistlib.loads(self.strings.read_bytes())
        overlay = json.loads((ROOT / "Assets/Sparkle.en.json").read_text())
        self.assertEqual(actual, self.original | overlay)
        self.assertEqual(actual["Install Update"], "Update")
        self.assertEqual(actual["Remind Me Later"], "Not Now")
        self.assertEqual(actual["Skip This Version"], self.original["Skip This Version"])
        self.assertEqual(actual["Install on Quit"], self.original["Install on Quit"])
        copy.customize(self.framework)
        self.assertEqual(plistlib.loads(self.strings.read_bytes()), actual)

    def test_vendor_key_change_fails_without_mutating_copy(self):
        del self.original["A new version of %@ is available!"]
        before = plistlib.dumps(self.original, fmt=plistlib.FMT_BINARY)
        self.strings.write_bytes(before)
        with self.assertRaisesRegex(ValueError, "review the English overlay"):
            copy.customize(self.framework)
        self.assertEqual(self.strings.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
