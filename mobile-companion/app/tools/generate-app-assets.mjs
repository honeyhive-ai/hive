// Renders the mobile app's launcher and splash art from the Hive brand geometry.
//
// The rosette, palette and PNG writer are ported from assets/branding/
// generate-brand-assets.mjs at the repo root, which is a pure script with no
// exports — so the primitives are duplicated here rather than imported. Keep
// the palette and cell layout in sync if the brand mark ever changes.
//
// Run with `bun run generate-assets` from mobile-companion/app.
//
// Three outputs, each 1024x1024:
//   assets/icon.png          full-bleed opaque tile — iOS/Android mask it themselves
//   assets/adaptive-icon.png Android foreground, transparent, inside the safe zone
//   assets/splash.png        transparent mark for the expo-splash-screen plugin
//
// icon.png sits on the dark brand tile, so it uses the stock cream cells. The
// other two sit on the cream #F9F3E4 background set in app.json, where cream
// cells would be invisible — those render the "ink" variant, which swaps the
// cells to the dark tile browns and the lit cells to honey.

import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const OUT_DIR = path.join(__dirname, "..", "assets");

const brand = {
  honeyLight: "#f0c25a",
  honeyDark: "#c8881f",
  tileTop: "#9a6620",
  tileBottom: "#4a2e12",
  cellLight: "#fbf1d8",
  cellMid: "#f0e0b8",
  activeLight: "#f0b878",
  activeDark: "#d98a44",
};

const SQ = 0.8660254;

// Cell radius and gap in mark units; the rosette spans MARK_UNITS across.
const CELL_R = 14;
const GAP_SCALE = 0.9;
// Widest extent: ring cell at 0deg (sqrt(3)*14) plus a hex half-width.
const MARK_UNITS = 2 * (Math.sqrt(3) * CELL_R + SQ * CELL_R * GAP_SCALE);

// Supersample factor. The brand icon renderer aliases hard; downsampling from
// 3x keeps the hex edges clean at launcher sizes.
const SS = 3;

function parseHexColor(value) {
  const n = value.replace("#", "");
  return {
    r: Number.parseInt(n.slice(0, 2), 16),
    g: Number.parseInt(n.slice(2, 4), 16),
    b: Number.parseInt(n.slice(4, 6), 16),
  };
}

function lerpColor(from, to, t) {
  return {
    r: Math.round(from.r + (to.r - from.r) * t),
    g: Math.round(from.g + (to.g - from.g) * t),
    b: Math.round(from.b + (to.b - from.b) * t),
  };
}

function gradientT(u, v, gx, gy) {
  const dot = u * gx + v * gy;
  return Math.max(0, Math.min(1, dot / (gx * gx + gy * gy)));
}

function cells() {
  const distance = Math.sqrt(3) * CELL_R;
  const litAngles = new Set([0, 240]);
  const result = [{ cx: 0, cy: 0, active: true, ord: -1 }];
  for (let degree = 0, ord = 0; degree < 360; degree += 60, ord += 1) {
    const angle = (degree * Math.PI) / 180;
    result.push({
      cx: distance * Math.cos(angle),
      cy: distance * Math.sin(angle),
      active: litAngles.has(degree),
      ord,
    });
  }
  return result;
}

function buildScaledHexPoints(cx, cy, scale, offsetX, offsetY) {
  const size = CELL_R * GAP_SCALE;
  return [
    [offsetX + scale * cx, offsetY + scale * (cy - size)],
    [offsetX + scale * (cx + SQ * size), offsetY + scale * (cy - 0.5 * size)],
    [offsetX + scale * (cx + SQ * size), offsetY + scale * (cy + 0.5 * size)],
    [offsetX + scale * cx, offsetY + scale * (cy + size)],
    [offsetX + scale * (cx - SQ * size), offsetY + scale * (cy + 0.5 * size)],
    [offsetX + scale * (cx - SQ * size), offsetY + scale * (cy - 0.5 * size)],
  ];
}

function pointInPolygon(x, y, polygon) {
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const [xi, yi] = polygon[i];
    const [xj, yj] = polygon[j];
    const intersect =
      yi > y !== yj > y &&
      x < ((xj - xi) * (y - yi)) / (yj - yi + Number.EPSILON) + xi;
    if (intersect) inside = !inside;
  }
  return inside;
}

function crc32(buffer) {
  let crc = 0xffffffff;
  for (const byte of buffer) {
    crc ^= byte;
    for (let i = 0; i < 8; i += 1) {
      const mask = -(crc & 1);
      crc = (crc >>> 1) ^ (0xedb88320 & mask);
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const typeBuffer = Buffer.from(type, "ascii");
  const lengthBuffer = Buffer.alloc(4);
  lengthBuffer.writeUInt32BE(data.length, 0);
  const crcBuffer = Buffer.alloc(4);
  crcBuffer.writeUInt32BE(crc32(Buffer.concat([typeBuffer, data])), 0);
  return Buffer.concat([lengthBuffer, typeBuffer, data, crcBuffer]);
}

function writeRgbaPng(pathname, size, pixels) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const stride = size * 4;
  const raw = Buffer.alloc(size * (stride + 1));
  for (let y = 0; y < size; y += 1) {
    const rowOffset = y * (stride + 1);
    raw[rowOffset] = 0; // filter: none
    pixels.copy(raw, rowOffset + 1, y * stride, (y + 1) * stride);
  }
  writeFileSync(
    pathname,
    Buffer.concat([
      Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
      pngChunk("IHDR", ihdr),
      pngChunk("IDAT", zlib.deflateSync(raw)),
      pngChunk("IEND", Buffer.alloc(0)),
    ]),
  );
}

// Average each SS x SS block. Colours are premultiplied by alpha before the
// average so transparent pixels don't drag a dark fringe into the mark's edge.
function downsample(hi, size) {
  const out = Buffer.alloc(size * size * 4, 0);
  const hiStride = size * SS * 4;
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      let sr = 0;
      let sg = 0;
      let sb = 0;
      let sa = 0;
      for (let dy = 0; dy < SS; dy += 1) {
        for (let dx = 0; dx < SS; dx += 1) {
          const o = (y * SS + dy) * hiStride + (x * SS + dx) * 4;
          const a = hi[o + 3];
          sr += hi[o] * a;
          sg += hi[o + 1] * a;
          sb += hi[o + 2] * a;
          sa += a;
        }
      }
      const o = (y * size + x) * 4;
      if (sa > 0) {
        out[o] = Math.round(sr / sa);
        out[o + 1] = Math.round(sg / sa);
        out[o + 2] = Math.round(sb / sa);
        out[o + 3] = Math.round(sa / (SS * SS));
      }
    }
  }
  return out;
}

/**
 * @param size      output edge length in px
 * @param markWidth mark width as a fraction of the canvas
 * @param variant   "tile" = cream cells on an opaque brand tile;
 *                  "ink"  = dark cells, transparent background
 */
function renderMark(pathname, { size, markWidth, variant }) {
  const W = size * SS;
  const hi = Buffer.alloc(W * W * 4, 0);

  const tileTop = parseHexColor(brand.tileTop);
  const tileBottom = parseHexColor(brand.tileBottom);
  const onTile = variant === "tile";
  const activeTop = parseHexColor(onTile ? brand.activeLight : brand.honeyLight);
  const activeBottom = parseHexColor(onTile ? brand.activeDark : brand.honeyDark);
  const restA = parseHexColor(onTile ? brand.cellLight : brand.tileTop);
  const restB = parseHexColor(onTile ? brand.cellMid : brand.tileBottom);

  const scale = (markWidth * W) / MARK_UNITS;
  const polygons = cells().map((cell) => {
    const points = buildScaledHexPoints(cell.cx, cell.cy, scale, W / 2, W / 2);
    const xs = points.map(([x]) => x);
    const ys = points.map(([, y]) => y);
    return {
      ...cell,
      points,
      minX: Math.min(...xs),
      maxX: Math.max(...xs),
      minY: Math.min(...ys),
      maxY: Math.max(...ys),
    };
  });

  for (let y = 0; y < W; y += 1) {
    for (let x = 0; x < W; x += 1) {
      const px = x + 0.5;
      const py = y + 0.5;
      let color = null;

      if (onTile) {
        color = lerpColor(tileTop, tileBottom, gradientT(px / W, py / W, 0.5, 1));
      }

      for (const polygon of polygons) {
        // Cheap bbox reject first — most pixels miss every cell.
        if (px < polygon.minX || px > polygon.maxX) continue;
        if (py < polygon.minY || py > polygon.maxY) continue;
        if (!pointInPolygon(px, py, polygon.points)) continue;
        if (polygon.active) {
          const u = (px - polygon.minX) / (polygon.maxX - polygon.minX);
          const v = (py - polygon.minY) / (polygon.maxY - polygon.minY);
          color = lerpColor(activeTop, activeBottom, gradientT(u, v, 0.4, 1));
        } else {
          color = polygon.ord % 2 ? restB : restA;
        }
      }

      if (!color) continue;
      const o = (y * W + x) * 4;
      hi[o] = color.r;
      hi[o + 1] = color.g;
      hi[o + 2] = color.b;
      hi[o + 3] = 255;
    }
  }

  writeRgbaPng(pathname, size, downsample(hi, size));
  return pathname;
}

mkdirSync(OUT_DIR, { recursive: true });

// Full bleed and opaque: both platforms apply their own mask, and an iOS app
// icon with alpha is rejected at submission.
renderMark(path.join(OUT_DIR, "icon.png"), {
  size: 1024,
  markWidth: 0.46,
  variant: "tile",
});

// Android composites this over adaptiveIcon.backgroundColor and shows only the
// central ~66/108 of the canvas, so the mark stays well inside that circle.
renderMark(path.join(OUT_DIR, "adaptive-icon.png"), {
  size: 1024,
  markWidth: 0.52,
  variant: "ink",
});

// Drawn at `imageWidth` dp by the splash plugin, so the mark fills more of its
// own canvas here — the surrounding transparency would otherwise be wasted.
renderMark(path.join(OUT_DIR, "splash.png"), {
  size: 1024,
  markWidth: 0.72,
  variant: "ink",
});

console.log("Generated app assets in", OUT_DIR);
