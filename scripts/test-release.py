#!/usr/bin/env python3
import base64
import copy
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("validate-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

# The same vectors are tested in xtask/src/version.rs.
VECTORS = [
    ("1.0.0-alpha.0", "1000000"),
    ("1.0.0-alpha.29", "1000029"),
    ("1.0.0-beta.2", "1000032"),
    ("1.0.0-rc.1", "1000061"),
    ("1.0.0", "1000099"),
    ("1.0.1", "1000199"),
    ("1.2.3", "1020399"),
    ("2.0.0-alpha.0", "2000000"),
    ("0.9.15", "91599"),
]


def template():
    return plistlib.loads((ROOT / "Config/Info.plist").read_bytes())


class VersionMapping(unittest.TestCase):
    def test_vectors(self):
        for version, build in VECTORS:
            with self.subTest(version=version):
                self.assertEqual(release.bundle_version(version), (version, build))

    def test_semver_order_is_build_order(self):
        ordered = ["1.0.0-alpha.0", "1.0.0-alpha.29", "1.0.0-beta.0", "1.0.0-rc.38", "1.0.0",
                   "1.0.1-alpha.0", "1.0.1", "1.99.99", "2.0.0-alpha.0"]
        builds = [int(release.bundle_version(version)[1]) for version in ordered]
        self.assertEqual(builds, sorted(set(builds)))
        # The Swift app's last build number was 27 (v0.9.15).
        self.assertGreater(builds[0], 27)
        keys = [release.version_key("v" + version) for version in ordered]
        self.assertEqual(keys, sorted(set(keys)))

    def test_unmappable_versions(self):
        for version in ("1.0", "1.0.0.0", "01.0.0", "1.100.0", "1.0.100", "1.0.0-dev", "1.0.0-alpha",
                        "1.0.0-alpha.1.2", "1.0.0-alpha.30", "1.0.0-rc.39", "1.0.0+5", "0.0.0", "1.0.0-beta.x"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                release.bundle_version(version)

    def test_workspace_version_is_read_from_cargo_toml(self):
        cargo = (ROOT / "Cargo.toml").read_text()
        version = release.workspace_version(cargo)
        release.bundle_version(version)
        text = '[package]\nversion = "9.9.9"\n\n[workspace.package]\nedition = "2024"\nversion = "1.2.3"\n\n[x]\nversion = "0.0.1"\n'
        self.assertEqual(release.workspace_version(text), "1.2.3")
        with self.assertRaises(ValueError):
            release.workspace_version('[package]\nversion = "1.0.0"\n')

    def test_template_uses_placeholders(self):
        info = template()
        self.assertEqual(info["CFBundleShortVersionString"], release.SHORT_VERSION_PLACEHOLDER)
        self.assertEqual(info["CFBundleVersion"], release.BUILD_NUMBER_PLACEHOLDER)
        stamped = release.load_info((ROOT / "Config/Info.plist").read_bytes(), (ROOT / "Cargo.toml").read_text())
        self.assertNotIn("$(", stamped["CFBundleShortVersionString"] + stamped["CFBundleVersion"])
        with self.assertRaises(ValueError):
            release.stamp(dict(info, CFBundleVersion="28"), "1.0.0")

    def test_literal_plists_of_swift_releases_load_unchanged(self):
        info = dict(template(), CFBundleShortVersionString="0.9.15", CFBundleVersion="27")
        self.assertEqual(release.load_info(plistlib.dumps(info)), info)

    def test_bundle_must_match_stamped_template(self):
        info = release.stamp(template(), "1.0.0")
        release.validate_bundle(info, copy.deepcopy(info))
        for key, value in (("CFBundleVersion", "1000098"), ("SUFeedURL", "https://example.com"), ("Extra", True)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                release.validate_bundle(info, dict(info, **{key: value}))


class ReleaseChecks(unittest.TestCase):
    def setUp(self):
        self.info = release.stamp(template(), "1.0.0")
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

    def validate(self, tag=None, info=None, allow_prerelease=False):
        ET.ElementTree(self.root).write(self.feed)
        return release.validate_appcast(tag or self.tag, info or self.info, self.feed, self.archive, allow_prerelease)

    def test_valid_metadata(self):
        self.assertEqual(len(base64.b64decode(self.validate())), 64)

    def test_prerelease_only_in_dry_runs(self):
        info = release.stamp(template(), "1.0.0-alpha.0")
        for name, value in (("version", info["CFBundleVersion"]), ("shortVersionString", info["CFBundleShortVersionString"])):
            self.item.find(release.SPARKLE + name).text = value
        self.enclosure.set("url", f"https://github.com/{release.REPOSITORY}/releases/download/v1.0.0-alpha.0/{self.archive.name}")
        with self.assertRaisesRegex(ValueError, "stable version"):
            self.validate("v1.0.0-alpha.0", info)
        self.validate("v1.0.0-alpha.0", info, allow_prerelease=True)

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
        for tag in ("v1.0", "v1.0.0-beta", "v1.0.0-alpha.0", "v1.0.0;echo bad", "v01.0.0", "v999.0.0", "v0.9.15"):
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

    def test_first_rust_release_follows_last_swift_release(self):
        swift = dict(template(), CFBundleShortVersionString="0.9.15", CFBundleVersion="27")
        release.validate_previous_release(self.tag, self.info, "v0.9.15", swift)
        prerelease = release.stamp(template(), "1.0.0-rc.0")
        release.validate_previous_release("v1.0.0-rc.0", prerelease, "v0.9.15", swift)
        with self.assertRaises(ValueError):
            release.validate_previous_release("v1.0.0-rc.0", prerelease, "v1.0.0", self.info)

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
