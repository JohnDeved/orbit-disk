#!/usr/bin/env python3
"""Reject release metadata that would point the updater at an unusable archive."""
import argparse
import base64
import plistlib
import os
import re
import subprocess
from pathlib import Path
import xml.etree.ElementTree as ET

REPOSITORY = "JohnDeved/orbit-disk"
FEED_URL = f"https://github.com/{REPOSITORY}/releases/latest/download/appcast.xml"
APPLE_SECRETS = (
    "APPLE_CERTIFICATE_P12_BASE64", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY",
    "APPLE_API_KEY_P8", "APPLE_API_KEY_ID", "APPLE_API_ISSUER",
)
SPARKLE = "{http://www.andymatuschak.org/xml-namespaces/sparkle}"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def signing_mode(environment):
    present = [name for name in APPLE_SECRETS if environment.get(name)]
    if not present:
        return "adhoc"
    missing = sorted(set(APPLE_SECRETS) - set(present))
    require(not missing, "Incomplete Apple signing credentials; missing: " + ", ".join(missing))
    require(environment["APPLE_SIGNING_IDENTITY"].startswith("Developer ID Application:"), "APPLE_SIGNING_IDENTITY must be a Developer ID Application identity")
    return "apple"


def validate_config(tag, info):
    require(re.fullmatch(r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)", tag), "Tag must be a stable version such as v0.9.0")
    require(tag == "v" + info["CFBundleShortVersionString"], "Tag does not match Config/Info.plist version")
    require(re.fullmatch(r"[1-9]\d*", info["CFBundleVersion"]), "Build number must be a positive integer")
    require(info["SUFeedURL"] == FEED_URL, "Unexpected update feed URL")
    require(len(base64.b64decode(info["SUPublicEDKey"], validate=True)) == 32, "Invalid Sparkle public key")
    require(info.get("SUVerifyUpdateBeforeExtraction") is True, "Archives must be verified before extraction")
    require(info.get("SURequireSignedFeed") is True, "Update feeds must be signed")
    require(info.get("SUSignedFeedFailureExpirationInterval") == 0, "Signed feed failures must stay closed")
    require(info.get("SUAllowsAutomaticUpdates") is False, "Installing an update must require user choice")


def validate_previous_release(tag, info, previous_tag, previous_info=None):
    current = tuple(map(int, tag[1:].split(".")))
    previous = tuple(map(int, previous_tag[1:].split(".")))
    require(current > previous, f"Release must be newer than existing tag {previous_tag}")
    if previous_info is not None:
        require(int(info["CFBundleVersion"]) > int(previous_info["CFBundleVersion"]), f"Build number must increase beyond {previous_tag}")


def validate_tag_history(tag, info):
    tags = subprocess.check_output(["git", "tag", "--list", "v*"], text=True).splitlines()
    for previous in tags:
        if previous == tag or not re.fullmatch(r"v\d+\.\d+\.\d+", previous):
            continue
        result = subprocess.run(["git", "show", f"{previous}:Config/Info.plist"], capture_output=True)
        # Versions before the updater did not have a shared Info.plist.
        previous_info = plistlib.loads(result.stdout) if result.returncode == 0 else None
        validate_previous_release(tag, info, previous, previous_info)


def validate_appcast(tag, info, appcast, archive):
    validate_config(tag, info)
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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--plist", type=Path, default=Path(__file__).resolve().parents[1] / "Config/Info.plist")
    parser.add_argument("--check-tags", action="store_true")
    parser.add_argument("--signing-mode", action="store_true")
    parser.add_argument("--appcast", type=Path)
    parser.add_argument("--archive", type=Path)
    args = parser.parse_args()
    try:
        with args.plist.open("rb") as file:
            info = plistlib.load(file)
        validate_config(args.tag, info)
        if args.signing_mode:
            print(signing_mode(os.environ))
        if args.check_tags:
            validate_tag_history(args.tag, info)
        if args.appcast or args.archive:
            require(args.appcast is not None and args.archive is not None, "Supply both --appcast and --archive")
            print(validate_appcast(args.tag, info, args.appcast, args.archive))
    except (ValueError, KeyError, OSError, ET.ParseError) as error:
        parser.exit(1, f"Release validation failed: {error}\n")


if __name__ == "__main__":
    main()
