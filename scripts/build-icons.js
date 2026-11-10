import { mkdir, writeFile } from 'node:fs/promises';
import { deflateSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';

const directory = new URL('../assets/', import.meta.url);
const size = 256;
const table = Array.from({ length: 256 }, (_, n) => {
  for (let i = 0; i < 8; i++) n = (n & 1) ? 0xedb88320 ^ (n >>> 1) : n >>> 1;
  return n >>> 0;
});
function chunk(type, data) {
  const name = Buffer.from(type), length = Buffer.alloc(4), checksum = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  let crc = 0xffffffff;
  for (const byte of Buffer.concat([name, data])) crc = table[(crc ^ byte) & 255] ^ (crc >>> 8);
  checksum.writeUInt32BE((crc ^ 0xffffffff) >>> 0);
  return Buffer.concat([length, name, data, checksum]);
}
function insidePolygon(x, y, polygon) {
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const [xi, yi] = polygon[i], [xj, yj] = polygon[j];
    if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) inside = !inside;
  }
  return inside;
}
const arrow = [[64, 174], [142, 96], [94, 96], [94, 65], [195, 65], [195, 166], [164, 166], [164, 118], [86, 196]];
const pixels = Buffer.alloc(size * (size * 4 + 1));
for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
  const color = [0, 0, 0];
  let covered = 0;
  // 四倍采样，让任务栏和托盘缩小后仍保留平滑边缘。
  for (let sy = 0; sy < 2; sy++) for (let sx = 0; sx < 2; sx++) {
    const px = x + (sx + 0.5) / 2, py = y + (sy + 0.5) / 2;
    const dx = Math.max(52 - px, 0, px - 204), dy = Math.max(52 - py, 0, py - 204);
    if (dx * dx + dy * dy > 44 * 44 || px < 8 || px > 248 || py < 8 || py > 248) continue;
    covered++;
    const rgb = insidePolygon(px, py, arrow) ? [16, 51, 43] : [123, 239, 203];
    for (let c = 0; c < 3; c++) color[c] += rgb[c];
  }
  const offset = y * (size * 4 + 1) + 1 + x * 4;
  for (let c = 0; c < 3; c++) pixels[offset + c] = covered ? Math.round(color[c] / covered) : 0;
  pixels[offset + 3] = Math.round(covered * 255 / 4);
}
const header = Buffer.alloc(13);
header.writeUInt32BE(size, 0); header.writeUInt32BE(size, 4); header[8] = 8; header[9] = 6;
const png = Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', header), chunk('IDAT', deflateSync(pixels)), chunk('IEND', Buffer.alloc(0))]);
const icoHeader = Buffer.alloc(22);
icoHeader.writeUInt16LE(1, 2); icoHeader.writeUInt16LE(1, 4);
icoHeader.writeUInt16LE(1, 10); icoHeader.writeUInt16LE(32, 12);
icoHeader.writeUInt32LE(png.length, 14); icoHeader.writeUInt32LE(22, 18);
await mkdir(directory, { recursive: true });
await writeFile(new URL('icon.png', directory), png);
await writeFile(new URL('icon.ico', directory), Buffer.concat([icoHeader, png]));
console.log(`已生成桌面图标：${fileURLToPath(directory)}`);
