const fs = require('node:fs/promises');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const sharp = require('sharp');

async function main() {
  const svg = await fs.readFile(path.join(__dirname, 'OrbitDisk.svg'));
  const iconset = path.join(__dirname, 'OrbitDisk.iconset');
  await fs.mkdir(iconset, { recursive: true });
  for (const size of [16, 32, 128, 256, 512]) {
    for (const scale of [1, 2]) {
      const pixels = size * scale;
      const name = `icon_${size}x${size}${scale === 2 ? '@2x' : ''}.png`;
      await sharp(svg).resize(pixels, pixels).png().toFile(path.join(iconset, name));
    }
  }
  execFileSync('/usr/bin/iconutil', [
    '-c', 'icns', iconset, '-o', path.join(__dirname, 'OrbitDisk.icns')
  ], { stdio: 'inherit' });
}

main().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
