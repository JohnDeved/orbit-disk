const fs = require('node:fs/promises');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const sharp = require('sharp');

async function main() {
  // Small slots use simplified drawings: OrbitDisk-16.svg for 16 px, OrbitDisk-32.svg for 32 px.
  const source = async name => fs.readFile(path.join(__dirname, name));
  const svg = { main: await source('OrbitDisk.svg'), s32: await source('OrbitDisk-32.svg'), s16: await source('OrbitDisk-16.svg') };
  const iconset = path.join(__dirname, 'OrbitDisk.iconset');
  await fs.mkdir(iconset, { recursive: true });
  for (const size of [16, 32, 128, 256, 512]) {
    for (const scale of [1, 2]) {
      const pixels = size * scale;
      const name = `icon_${size}x${size}${scale === 2 ? '@2x' : ''}.png`;
      const art = pixels <= 16 ? svg.s16 : pixels <= 32 ? svg.s32 : svg.main;
      await sharp(art).resize(pixels, pixels).png().toFile(path.join(iconset, name));
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
