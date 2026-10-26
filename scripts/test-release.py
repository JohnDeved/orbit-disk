#!/usr/bin/env python3
import base64
import copy
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
import xml.etree.ElementTree as ET

spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("validate-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseChecks(unittest.TestCase):
    def setUp(self):
        with (Path(__file__).resolve().parents[1] / "Config/Info.plist").open("rb") as file:
            self.info = plistlib.load(file)
        self.tag = "v" + self.info["CFBundleShortVersionString"]
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.archive = Path(self.temp.name) / "Orbit-Disk-test-universal.zip"
        self.archive.write_bytes(b"test update archive")
        self.feed = Path(self.temp.name) / "appcast.xml"
        self.root = ET.Element("rss")
        self.item = ET.SubElement(ET.SubElement(self.root, "channel"), "item")
        for name, value in [("version", self.info["CFBundleVersion"]), ("shortVersionString", self.info["CFBundleShortVersionString"]), ("minimumSystemVersion", self.info["LSMinimumSystemVersion"])]:
            ET.SubElement(self.item, release.SPARKLE + name).text = value
        self.enclosure = ET.SubElement(self.item, "enclosure", {
            "url": f"https://github.com/{release.REPOSITORY}/releases/download/{self.tag}/{self.archive.name}",
            "length": str(self.archive.stat().st_size),
            release.SPARKLE + "edSignature": base64.b64encode(bytes(64)).decode(),
        })

    def validate(self):
        ET.ElementTree(self.root).write(self.feed)
        return release.validate_appcast(self.tag, self.info, self.feed, self.archive)

    def test_valid_metadata(self):
        self.assertEqual(len(base64.b64decode(self.validate())), 64)

    def test_no_apple_credentials_uses_adhoc(self):
        self.assertEqual(release.signing_mode({}), "adhoc")

    def test_partial_apple_credentials_fail(self):
        with self.assertRaisesRegex(ValueError, "Incomplete Apple signing credentials"):
            release.signing_mode({"APPLE_CERTIFICATE_P12_BASE64": "configured"})

    def test_complete_apple_credentials(self):
        environment = {name: "configured" for name in release.APPLE_SECRETS}
        environment["APPLE_SIGNING_IDENTITY"] = "Developer ID Application: Example (TEAMID)"
        self.assertEqual(release.signing_mode(environment), "apple")
        environment["APPLE_SIGNING_IDENTITY"] = "Apple Development: Example"
        with self.assertRaisesRegex(ValueError, "Developer ID Application"):
            release.signing_mode(environment)

    def test_invalid_tags(self):
        for tag in ("v0.9", "v0.9.0-beta", "v0.9.0;echo bad", "v00.9.0", "v999.0.0"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                release.validate_config(tag, self.info)

    def test_security_config(self):
        for key, value in (("SUPublicEDKey", "placeholder"), ("SURequireSignedFeed", False), ("SUAllowsAutomaticUpdates", True), ("SUVerifyUpdateBeforeExtraction", False), ("SUSignedFeedFailureExpirationInterval", 86400), ("SUFeedURL", "http://example.com/feed")):
            info = copy.deepcopy(self.info)
            info[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                release.validate_config(self.tag, info)

    def test_monotonic_versions_and_builds(self):
        release.validate_previous_release("v1.1.0", {"CFBundleVersion": "13"}, "v1.0.0", {"CFBundleVersion": "12"})
        for tag, build in (("v0.9.0", "13"), ("v1.0.0", "13"), ("v1.1.0", "12")):
            with self.subTest(tag=tag, build=build), self.assertRaises(ValueError):
                release.validate_previous_release(tag, {"CFBundleVersion": build}, "v1.0.0", {"CFBundleVersion": "12"})

    def test_missing_signature(self):
        del self.enclosure.attrib[release.SPARKLE + "edSignature"]
        with self.assertRaises(ValueError):
            self.validate()

    def test_wrong_download_url(self):
        self.enclosure.set("url", "https://example.com/update.zip")
        with self.assertRaises(ValueError):
            self.validate()

    def test_changed_archive(self):
        self.archive.write_bytes(b"modified archive")
        with self.assertRaises(ValueError):
            self.validate()

    def test_wrong_build(self):
        self.item.find(release.SPARKLE + "version").text = "1"
        with self.assertRaises(ValueError):
            self.validate()

    def test_extra_release(self):
        self.root.find("channel").append(copy.deepcopy(self.item))
        with self.assertRaises(ValueError):
            self.validate()

    def test_deltas_rejected(self):
        ET.SubElement(self.item, release.SPARKLE + "deltas")
        with self.assertRaises(ValueError):
            self.validate()


if __name__ == "__main__":
    unittest.main()
