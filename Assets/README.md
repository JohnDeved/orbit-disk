# Orbit Disk icon

`OrbitDisk.svg` is the editable source for the approved Night icon. Its charcoal tile, coral disc, ivory orbital ring, satin lines, bevels and contact shadow are rendered without cropping or flattening the transparent margins.

`OrbitDisk.icns` contains the ten macOS icon variants: 16, 32, 128, 256 and 512 points at 1× and 2×. Normal app builds copy that resource and require only Apple's build tools. The renderer also creates an ignored `OrbitDisk.iconset` folder for inspection; those intermediate PNGs need not be committed.

The 1024-unit canvas centers the tile and artwork at (512, 512). The tile is 832 × 832 with 192-unit corner radii; the disc radius is 224. The orbit uses a shared ellipse with radii 360 × 128, a 48-unit outer stroke, a 44-unit face and a 24-degree tilt. Its front and rear layers preserve the same contour. Opposing margins remain equal.

After editing the SVG, regenerate on macOS with Node.js and Sharp available:

```sh
node Assets/render-icon.cjs
./build-app.sh
```

The approved renders were generated with Sharp 0.35.5 and librsvg 2.63.2. The script renders each size directly from the same SVG and uses macOS `iconutil` to package the PNGs. It preserves the source geometry, colors and alpha channel. No Node dependency is used by the app.
