#!/usr/bin/env node
/**
 * 曲栏 Barline —— 占位图标生成器（M1 临时用，M5 会换成正式设计稿）
 *
 * 产出 `assets/icon.ico`：
 *   - 多尺寸 16 / 20 / 24 / 32 / 48 / 256
 *   - 未压缩 BGRA DIB 格式（BITMAPINFOHEADER + XOR 位图 + AND 掩码），
 *     这是 Windows 上兼容性最好的 32bpp ICO 变体，rc.exe / Explorer / Shell_NotifyIconW 都认。
 *   - 视觉：深色圆角底 + 一个简单音符
 *
 * 只依赖 Node 内置模块，4×4 超采样做抗锯齿。
 *
 * 用法：node scripts/gen-icon.mjs [输出路径]
 */

import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DEFAULT_OUT = resolve(HERE, '..', 'assets', 'icon.ico');

const SIZES = [16, 20, 24, 32, 48, 256];
const SS = 4; // 每个像素的每轴超采样数

// ----------------------------------------------------------------- 配色
const BG_TOP = [0x24, 0x24, 0x33]; // 圆角底上半
const BG_BOTTOM = [0x14, 0x14, 0x1d]; // 圆角底下半
const ACCENT = [0x6c, 0xd4, 0xff]; // 音符主色
const NOTE_SHADOW = [0x18, 0x18, 0x24];

// ----------------------------------------------------------------- 几何
/** 圆角矩形内部判定（归一化坐标 0..1）。 */
function insideRoundRect(x, y, x0, y0, x1, y1, r) {
  if (x < x0 || x > x1 || y < y0 || y > y1) return false;
  const cx = Math.min(Math.max(x, x0 + r), x1 - r);
  const cy = Math.min(Math.max(y, y0 + r), y1 - r);
  const dx = x - cx;
  const dy = y - cy;
  return dx * dx + dy * dy <= r * r;
}

/** 椭圆内部判定。 */
function insideEllipse(x, y, cx, cy, rx, ry) {
  const dx = (x - cx) / rx;
  const dy = (y - cy) / ry;
  return dx * dx + dy * dy <= 1;
}

/**
 * 采样一个点，返回 [r,g,b,a]（直通 alpha，非预乘）。
 * 坐标归一化到 0..1。
 */
function sample(x, y) {
  // 1) 圆角底
  const inBg = insideRoundRect(x, y, 0.02, 0.02, 0.98, 0.98, 0.235);
  if (!inBg) return [0, 0, 0, 0];

  // 底色：竖直渐变
  const t = Math.min(Math.max((y - 0.02) / 0.96, 0), 1);
  let r = Math.round(BG_TOP[0] + (BG_BOTTOM[0] - BG_TOP[0]) * t);
  let g = Math.round(BG_TOP[1] + (BG_BOTTOM[1] - BG_TOP[1]) * t);
  let b = Math.round(BG_TOP[2] + (BG_BOTTOM[2] - BG_TOP[2]) * t);
  let a = 255;

  // 2) 音符：符头（椭圆）+ 符干（竖条）+ 符尾（斜三角）
  const head = insideEllipse(x, y, 0.385, 0.735, 0.175, 0.135);
  const stem = x >= 0.535 && x <= 0.615 && y >= 0.245 && y <= 0.735;
  // 符尾：从符干顶端向右下的一条渐薄斜带
  const flagT = (y - 0.245) / 0.30;
  const flag =
    y >= 0.245 &&
    y <= 0.545 &&
    x >= 0.535 &&
    x <= 0.535 + 0.27 * (1 - flagT) + 0.02 &&
    x <= 0.815;

  const inNote = head || stem || flag;
  if (inNote) {
    r = ACCENT[0];
    g = ACCENT[1];
    b = ACCENT[2];
    a = 255;
  }

  // 3) 音符描边（在音符外侧 1.2% 环带上压暗，小尺寸下让轮廓更清晰）
  const ringHead = insideEllipse(x, y, 0.385, 0.735, 0.175 + 0.035, 0.135 + 0.035) && !head;
  const ringStem =
    x >= 0.535 - 0.03 && x <= 0.615 + 0.03 && y >= 0.245 - 0.03 && y <= 0.735 + 0.03 && !stem;
  if (!inNote && (ringHead || ringStem)) {
    r = NOTE_SHADOW[0];
    g = NOTE_SHADOW[1];
    b = NOTE_SHADOW[2];
  }

  return [r, g, b, a];
}

/** 生成 size×size 的 RGBA（非预乘）像素数组，行优先、自上而下。 */
function renderRGBA(size) {
  const out = new Uint8Array(size * size * 4);
  const inv = 1 / (size * SS);
  for (let py = 0; py < size; py += 1) {
    for (let px = 0; px < size; px += 1) {
      let sr = 0;
      let sg = 0;
      let sb = 0;
      let sa = 0;
      for (let sy = 0; sy < SS; sy += 1) {
        for (let sx = 0; sx < SS; sx += 1) {
          const x = (px * SS + sx + 0.5) * inv;
          const y = (py * SS + sy + 0.5) * inv;
          const [r, g, b, a] = sample(x, y);
          // 按 alpha 加权累加颜色，避免边缘出现「暗边」
          const w = a / 255;
          sr += r * w;
          sg += g * w;
          sb += b * w;
          sa += a;
        }
      }
      const n = SS * SS;
      const alpha = Math.round(sa / n);
      const o = (py * size + px) * 4;
      if (alpha === 0) {
        out[o] = 0;
        out[o + 1] = 0;
        out[o + 2] = 0;
        out[o + 3] = 0;
      } else {
        const wsum = sa / 255; // 加权系数和
        out[o] = Math.min(255, Math.round(sr / wsum));
        out[o + 1] = Math.min(255, Math.round(sg / wsum));
        out[o + 2] = Math.min(255, Math.round(sb / wsum));
        out[o + 3] = alpha;
      }
    }
  }
  return out;
}

// ----------------------------------------------------------------- DIB 编码
/** 把 RGBA（自上而下）编成 ICO 的 DIB 图像块：BITMAPINFOHEADER + XOR + AND。 */
function encodeDIB(size, rgba) {
  const xorStride = size * 4;
  const andStride = ((size + 31) >> 5) << 2; // 1bpp，行按 4 字节对齐
  const headerSize = 40;
  const xorSize = xorStride * size;
  const andSize = andStride * size;
  const buf = Buffer.alloc(headerSize + xorSize + andSize, 0);

  // ---- BITMAPINFOHEADER（高度 = 2 × size，含 AND 掩码） ----
  buf.writeUInt32LE(headerSize, 0); // biSize
  buf.writeInt32LE(size, 4); // biWidth
  buf.writeInt32LE(size * 2, 8); // biHeight（XOR + AND）
  buf.writeUInt16LE(1, 12); // biPlanes
  buf.writeUInt16LE(32, 14); // biBitCount
  buf.writeUInt32LE(0, 16); // biCompression = BI_RGB
  buf.writeUInt32LE(xorSize, 20); // biSizeImage
  buf.writeInt32LE(0, 24); // biXPelsPerMeter
  buf.writeInt32LE(0, 28); // biYPelsPerMeter
  buf.writeUInt32LE(0, 32); // biClrUsed
  buf.writeUInt32LE(0, 36); // biClrImportant

  // ---- XOR 位图：BGRA，底行优先（自下而上） ----
  for (let y = 0; y < size; y += 1) {
    const srcRow = size - 1 - y; // 翻转
    for (let x = 0; x < size; x += 1) {
      const s = (srcRow * size + x) * 4;
      const d = headerSize + y * xorStride + x * 4;
      buf[d] = rgba[s + 2]; // B
      buf[d + 1] = rgba[s + 1]; // G
      buf[d + 2] = rgba[s]; // R
      buf[d + 3] = rgba[s + 3]; // A
    }
  }

  // ---- AND 掩码：全 0（「复制 XOR」）。32bpp 下系统按 alpha 走，但掩码必须存在 ----
  // 已经用 Buffer.alloc(...,0) 填零，无需额外写入。

  return buf;
}

// ----------------------------------------------------------------- ICO 组装
function buildICO(sizes) {
  const images = sizes.map((size) => encodeDIB(size, renderRGBA(size)));

  const dirSize = 6 + 16 * sizes.length;
  let offset = dirSize;
  const offsets = images.map((img) => {
    const cur = offset;
    offset += img.length;
    return cur;
  });

  const out = Buffer.alloc(offset, 0);
  out.writeUInt16LE(0, 0); // reserved
  out.writeUInt16LE(1, 2); // type = 1 (ICO)
  out.writeUInt16LE(sizes.length, 4); // count

  for (let i = 0; i < sizes.length; i += 1) {
    const size = sizes[i];
    const e = 6 + i * 16;
    out.writeUInt8(size >= 256 ? 0 : size, e + 0); // width（0 表示 256）
    out.writeUInt8(size >= 256 ? 0 : size, e + 1); // height
    out.writeUInt8(0, e + 2); // colorCount
    out.writeUInt8(0, e + 3); // reserved
    out.writeUInt16LE(1, e + 4); // planes
    out.writeUInt16LE(32, e + 6); // bitCount
    out.writeUInt32LE(images[i].length, e + 8); // bytesInRes
    out.writeUInt32LE(offsets[i], e + 12); // imageOffset
  }

  for (let i = 0; i < images.length; i += 1) {
    images[i].copy(out, offsets[i]);
  }

  return out;
}

// ----------------------------------------------------------------- main
const outPath = process.argv[2] ? resolve(process.argv[2]) : DEFAULT_OUT;
mkdirSync(dirname(outPath), { recursive: true });
const ico = buildICO(SIZES);
writeFileSync(outPath, ico);
console.log(`[gen-icon] ${outPath}  ${SIZES.join('/')}  ${ico.length} bytes`);
