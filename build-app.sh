#!/bin/zsh
set -euo pipefail
cd "${0:A:h}"
swift build -c release --product OrbitDisk
app_path="${PWD:h}/Orbit Disk.app"
mkdir -p "$app_path/Contents/MacOS" "$app_path/Contents/Resources"
cp Assets/OrbitDisk.icns "$app_path/Contents/Resources/OrbitDisk.icns"
cp .build/release/OrbitDisk "$app_path/Contents/MacOS/OrbitDisk.new"
mv -f "$app_path/Contents/MacOS/OrbitDisk.new" "$app_path/Contents/MacOS/OrbitDisk"
cat > "$app_path/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>OrbitDisk</string>
<key>CFBundleIdentifier</key><string>local.orbitdisk.app</string>
<key>CFBundleName</key><string>Orbit Disk</string>
<key>CFBundleDisplayName</key><string>Orbit Disk</string>
<key>CFBundleIconFile</key><string>OrbitDisk</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.8.3</string>
<key>CFBundleVersion</key><string>11</string>
<key>UTExportedTypeDeclarations</key><array><dict>
<key>UTTypeIdentifier</key><string>local.orbitdisk.collection-items</string>
<key>UTTypeDescription</key><string>Orbit Disk Collection Items</string>
<key>UTTypeConformsTo</key><array><string>public.data</string></array>
</dict></array>
<key>LSMinimumSystemVersion</key><string>14.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>NSHumanReadableCopyright</key><string>Orbit Disk, 2026</string>
</dict></plist>
PLIST
codesign --force --sign - "$app_path"
printf 'Built %s\n' "$app_path"
