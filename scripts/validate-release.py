#!/usr/bin/env python3
"""Reject release metadata that would point the updater at an unusable archive.

The app version comes from `[workspace.package] version` in Cargo.toml. Config/Info.plist holds
the placeholders $(ORBIT_SHORT_VERSION) and $(ORBIT_BUILD_NUMBER), which `cargo xtask bundle`
stamps (xtask/src/version.rs). This script applies the same mapping, so it can validate a tag
before building and cross-check the stamped Info.plist of the built app (--bundle).
"""
import argparse
import base64
import plistlib
import os
import re
import subprocess
from pathlib import Path
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY = "JohnDeved/orbit-disk"
FEED_URL = f"https://github.com/{REPOSITORY}/releases/latest/download/appcast.xml"
APPLE_SECRETS = (
    "APPLE_CERTIFICATE_P12_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY",
    "APPLE_API_KEY_P8", "APPLE_API_KEY_ID", "APPLE_API_ISSUER",
)
SPARKLE = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"
SHORT_VERSION_PLACEHOLDER = "$(ORBIT_SHORT_VERSION)"
BUILD_NUMBER_PLACEHOLDER = "$(ORBIT_BUILD_NUMBER)"
NUMBER = r"(0|[1-9]\d{0,5})"
STABLE_TAG = re.compile(rf"v{NUMBER}\.{NUMBER}\.{NUMBER}")
PRERELEASE_TAG = re.compile(rf"v{NUMBER}\.{NUMBER}\.{NUMBER}-(alpha|beta|rc)\.{NUMBER}")
STAGES = {"alpha": (0, 29), "beta": (30, 29), "rc": (60, 38)}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def bundle_version(version):
    """Maps a workspace semver to (CFBundleShortVersionString, CFBundleVersion); see xtask/src/version.rs."""
    match = re.fullmatch(rf"{NUMBER}\.{NUMBER}\.{NUMBER}(?:-(alpha|beta|rc)\.{NUMBER})?", version)
    require(match, f"Workspace version {version!r} must be MAJOR.MINOR.PATCH or MAJOR.MINOR.PATCH-(alpha|beta|rc).N")
    major, minor, patch = (int(match.group(index)) for index in (1, 2, 3))
    require(minor <= 99 and patch <= 99, "MINOR and PATCH must be at most 99")
    require((major, minor, patch) != (0, 0, 0), "0.0.0 has no build number")
    stage = 99
    if match.group(4):
        base, limit = STAGES[match.group(4)]
        number = int(match.group(5))
        require(number <= limit, f"{match.group(4)} number must be at most {limit}")
        stage = base + number
    return version, str(major * 1_000_000 + minor * 10_000 + patch * 100 + stage)


def workspace_version(cargo_toml):
    """`version` from the [workspace.package] table (Python 3.9 has no tomllib)."""
    section = re.search(r"^\[workspace\.package\]\s*$(.*?)(?=^\[|\Z)", cargo_toml, re.M | re.S)
    require(section, "Cargo.toml has no [workspace.package] table")
    version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', section.group(1), re.M)
    require(version, "[workspace.package] has no version")
    return version.group(1)


def stamp(info, version):
    """Fills the Info.plist placeholders; each must be present (as in xtask)."""
    require(info.get("CFBundleShortVersionString") == SHORT_VERSION_PLACEHOLDER
            and info.get("CFBundleVersion") == BUILD_NUMBER_PLACEHOLDER,
            "Config/Info.plist must use the version placeholders; the version lives in Cargo.toml")
    short, build = bundle_version(version)
    return dict(info, CFBundleShortVersionString=short, CFBundleVersion=build)


def load_info(plist_bytes, cargo_toml=None):
    """A release's effective Info.plist: placeholders stamped from Cargo.toml, or (releases
    before the Rust port) the literal values."""
    info = plistlib.loads(plist_bytes)
    if info.get("CFBundleShortVersionString") == SHORT_VERSION_PLACEHOLDER:
        require(cargo_toml is not None, "Placeholder Info.plist without Cargo.toml")
        return stamp(info, workspace_version(cargo_toml))
    return info


def signing_mode(environment):
    present = [name for name in APPLE_SECRETS if environment.get(name)]
    if not present:
        return "adhoc"
    missing = sorted(set(APPLE_SECRETS) - set(present))
    require(not missing, "Incomplete Apple signing credentials; missing: " + ", ".join(missing))
    require(environment["APPLE_SIGNING_IDENTITY"].startswith("Developer ID Application:"), "APPLE_SIGNING_IDENTITY must be a Developer ID Application identity")
    return "apple"


def validate_config(tag, info, allow_prerelease=False):
    stable = STABLE_TAG.fullmatch(tag)
    require(stable or (allow_prerelease and PRERELEASE_TAG.fullmatch(tag)), "Tag must be a stable version such as v0.9.0")
    require(tag == "v" + info["CFBundleShortVersionString"], "Tag does not match the app version (Cargo.toml [workspace.package] version)")
    require(re.fullmatch(r"[1-9]\d*", info["CFBundleVersion"]), "Build number must be a positive integer")
    require(info["SUFeedURL"] == FEED_URL, "Unexpected update feed URL")
    require(len(base64.b64decode(info["SUPublicEDKey"], validate=True)) == 32, "Invalid Sparkle public key")
    require(info.get("SUVerifyUpdateBeforeExtraction") is True, "Archives must be verified before extraction")
    require(info.get("SURequireSignedFeed") is True, "Update feeds must be signed")
    require(info.get("SUSignedFeedFailureExpirationInterval") == 0, "Signed feed failures must stay closed")
    require(info.get("SUAllowsAutomaticUpdates") is False, "Installing an update must require user choice")


def version_key(tag):
    """Semver order: a prerelease sorts before its stable release."""
    core, _, pre = tag[1:].partition("-")
    stage = 99
    if pre:
        label, number = pre.split(".")
        stage = STAGES[label][0] + int(number)
    return tuple(map(int, core.split("."))) + (stage,)


def validate_previous_release(tag, info, previous_tag, previous_info=None):
    require(version_key(tag) > version_key(previous_tag), f"Release must be newer than existing tag {previous_tag}")
    if previous_info is not None:
        require(int(info["CFBundleVersion"]) > int(previous_info["CFBundleVersion"]), f"Build number must increase beyond {previous_tag}")


def git_show(revision, path):
    result = subprocess.run(["git", "-C", str(ROOT), "show", f"{revision}:{path}"], capture_output=True)
    return result.stdout if result.returncode == 0 else None


def validate_tag_history(tag, info):
    tags = subprocess.check_output(["git", "-C", str(ROOT), "tag", "--list", "v*"], text=True).splitlines()
    for previous in tags:
        if previous == tag or not re.fullmatch(r"v\d+\.\d+\.\d+", previous):
            continue
        plist = git_show(previous, "Config/Info.plist")
        cargo = git_show(previous, "Cargo.toml")
        # Versions before the updater did not have a shared Info.plist.
        previous_info = load_info(plist, cargo.decode() if cargo else None) if plist is not None else None
        validate_previous_release(tag, info, previous, previous_info)


def validate_bundle(info, bundle_info):
    """The built app's Info.plist must be exactly the stamped template."""
    differences = sorted(key for key in set(info) | set(bundle_info) if info.get(key) != bundle_info.get(key))
    require(not differences, "Bundle Info.plist differs from the stamped Config/Info.plist: " + ", ".join(differences))


def validate_appcast(tag, info, appcast, archive, allow_prerelease=False):
    validate_config(tag, info, allow_prerelease)
    items = ET.parse(appcast).findall("channel/item")
    require(len(items) == 1, "Expected exactly one stable release")
    item = items[0]
    require(item.findtext(SPARKLE + "version") == info["CFBundleVersion"], "Feed build does not match app")
    require(item.findtext(SPARKLE + "shortVersionString") == info["CFBundleShortVersionString"], "Feed version does not match app")
    require(item.findtext(SPARKLE + "minimumSystemVersion") == info["LSMinimumSystemVersion"], "Feed system requirement does not match app")
    enclosures = item.findall("enclosure")
    require(len(enclosures) == 1, "Expected exactly one update archive")
    enclosure = enclosures[0]
    require(enclosure.get("url") == f"https://github.com/{REPOSITORY}/releases/download/{tag}/{archive.name}", "Unexpected update download URL")
    require(enclosure.get("length") == str(archive.stat().st_size), "Archive length does not match feed")
    signature = enclosure.get(SPARKLE + "edSignature", "")
    require(len(base64.b64decode(signature, validate=True)) == 64, "Archive EdDSA signature missing or malformed")
    require(item.find(SPARKLE + "deltas") is None, "Delta updates are not published")
    return signature


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("tag", nargs="?", help="vX.Y.Z; omit with --signing-mode or --print-version")
    parser.add_argument("--plist", type=Path, default=ROOT / "Config/Info.plist")
    parser.add_argument("--cargo-toml", type=Path, default=ROOT / "Cargo.toml")
    parser.add_argument("--bundle", type=Path, help="built app whose Info.plist must equal the stamped template")
    parser.add_argument("--check-tags", action="store_true")
    parser.add_argument("--signing-mode", action="store_true")
    parser.add_argument("--print-version", action="store_true", help="print 'SHORT BUILD' for the workspace version")
    parser.add_argument("--allow-prerelease", action="store_true", help="dry runs only: accept vX.Y.Z-(alpha|beta|rc).N")
    parser.add_argument("--appcast", type=Path)
    parser.add_argument("--archive", type=Path)
    args = parser.parse_args()
    try:
        info = load_info(args.plist.read_bytes(), args.cargo_toml.read_text())
        if args.print_version:
            print(info["CFBundleShortVersionString"], info["CFBundleVersion"])
        if args.tag is None:
            require(args.signing_mode or args.print_version, "A tag is required")
            require(not (args.check_tags or args.bundle or args.appcast or args.archive), "A tag is required")
            if args.signing_mode:
                print(signing_mode(os.environ))
            return
        validate_config(args.tag, info, args.allow_prerelease)
        if args.signing_mode:
            print(signing_mode(os.environ))
        if args.check_tags:
            validate_tag_history(args.tag, info)
        if args.bundle:
            validate_bundle(info, plistlib.loads((args.bundle / "Contents/Info.plist").read_bytes()))
        if args.appcast or args.archive:
            require(args.appcast is not None and args.archive is not None, "Supply both --appcast and --archive")
            print(validate_appcast(args.tag, info, args.appcast, args.archive, args.allow_prerelease))
    except (ValueError, KeyError, OSError, ET.ParseError, plistlib.InvalidFileException) as error:
        parser.exit(1, f"Release validation failed: {error}\n")


if __name__ == "__main__":
    main()
