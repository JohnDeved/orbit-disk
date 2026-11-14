# Orbit Disk icon

The logo is a charcoal planet with a coral slice lifted out of its outer band, and a slate ring that passes behind the planet and across its front. It comes in three versions, all drawn from the same outlines on a 1024-unit canvas:

| File | Use |
|---|---|
| `OrbitDisk.svg` | The macOS app icon: the mark on the warm-white tile, with the tile gradient, rim light, drop shadow, vertical shading and a soft glow under the slice. It is the source of `OrbitDisk.icns`. |
| `OrbitDisk-flat.svg` | The same app icon without effects: four flat colours, no gradients, filters or masks. Use it where effects don't reproduce (print, small embeds). |
| `OrbitDisk-32.svg`, `OrbitDisk-16.svg` | Simplified drawings of the app icon for the 32 px and 16 px slots of the `.icns`: a thicker ring and wider gaps at 32 px; at 16 px the planet and ring merge into one silhouette. |
| `OrbitDisk-mark.svg` | The mark alone, without the tile, in three flat colours. Use it everywhere outside the Dock and Finder: README, website, docs, slides. |

The geometry: the planet has radius 232; its outer band (radius 110–232) is cut away between −150° and −90° (edges at 30° and 90°), and that 60° segment, with 10-unit rounded corners, is moved 88 units out along its bisector. The cutout sits above the point where the ring passes behind the planet, so the two never touch. The ring is the band between two ellipses (350 × 128, ±28) tilted −20°; its back half is hidden behind the planet disk, and the planet has a 26-unit gap on each side of the front half. The mark is scaled to 93 %, centred on its bounding box and raised 1.2 % of the tile for the optical centre. Every part is a filled outline (no strokes or masks). `OrbitDisk-flat.svg` and `OrbitDisk-mark.svg` pass the logo-design skill's SVG audit with no findings. The ring is drawn thick (56 units) and in slate so it stays an unbroken line at Dock sizes (64 px and below).

Colours: charcoal `#2B2F3B`, slate `#4A4F5E`, coral `#F4734A`, tile `#F7F4EF` (flat versions); the app icon shades them from `#3C4152`→`#1F222C`, `#5A5F6E`→`#3E4250` and `#FFA780`→`#F36A40` on a `#FFFFFF`→`#EEE9E1` tile.

`OrbitDisk.icns` contains the ten macOS icon variants: 16, 32, 128, 256 and 512 points at 1× and 2×. The 16 px variant is rendered from `OrbitDisk-16.svg`, the two 32 px variants from `OrbitDisk-32.svg`, and the rest from `OrbitDisk.svg`. Normal app builds copy that resource and require only Apple's build tools. The renderer also creates an ignored `OrbitDisk.iconset` folder for inspection; those intermediate PNGs need not be committed.

After editing the SVGs, regenerate on macOS with Node.js and Sharp available:

```sh
node Assets/render-icon.cjs
./build-app.sh
```

The current renders were generated with Sharp 0.35.5. The script renders each size directly from its SVG and uses macOS `iconutil` to package the PNGs. It preserves the source geometry, colours and alpha channel. No Node dependency is used by the app.
