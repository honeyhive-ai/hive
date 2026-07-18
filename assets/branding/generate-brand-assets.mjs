import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const palette = {
  studio: {
    bg: "#f4f1ea",
    panel: "#ffffff",
    ink: "#221f1a",
    soft: "#8a8073",
    line: "rgba(40,30,20,.12)",
    honeyLight: "#f0c25a",
    honeyDark: "#c8881f",
    honeyDeep: "#8a5a14",
    warmLight: "#e3a460",
    warmDark: "#c6713a",
    tileTop: "#9a6620",
    tileBottom: "#4a2e12",
    tileTopDark: "#3a2812",
    tileBottomDark: "#1a1206",
    cellLight: "#fbf1d8",
    cellMid: "#f0e0b8",
    activeLight: "#f0b878",
    activeDark: "#d98a44",
  },
};

const SQ = 0.8660254;

// Pointy-top hexagon vertices, center (cx,cy), radius R (vertex distance).
function hexPolygon(cx, cy, R) {
  return [
    [cx, cy - R],
    [cx + SQ * R, cy - 0.5 * R],
    [cx + SQ * R, cy + 0.5 * R],
    [cx, cy + R],
    [cx - SQ * R, cy + 0.5 * R],
    [cx - SQ * R, cy - 0.5 * R],
  ];
}

function hexPathString(cx, cy, R) {
  return `M${hexPolygon(cx, cy, R)
    .map(([x, y]) => `${x.toFixed(2)} ${y.toFixed(2)}`)
    .join(" L ")} Z`;
}

function hex(cx, cy, radius, gapScale = 0.9) {
  const size = radius * gapScale;
  const points = [
    [0, -size],
    [SQ * size, -0.5 * size],
    [SQ * size, 0.5 * size],
    [0, size],
    [-SQ * size, 0.5 * size],
    [-SQ * size, -0.5 * size],
  ];

  return `M${points
    .map(([dx, dy]) => `${(cx + dx).toFixed(2)} ${(cy + dy).toFixed(2)}`)
    .join(" L ")} Z`;
}

function cells(radius) {
  const distance = Math.sqrt(3) * radius;
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

function parseHexColor(value) {
  const normalized = value.replace("#", "");
  return {
    r: Number.parseInt(normalized.slice(0, 2), 16),
    g: Number.parseInt(normalized.slice(2, 4), 16),
    b: Number.parseInt(normalized.slice(4, 6), 16),
  };
}

function lerpChannel(from, to, t) {
  return Math.round(from + (to - from) * t);
}

function lerpColor(from, to, t) {
  return {
    r: lerpChannel(from.r, to.r, t),
    g: lerpChannel(from.g, to.g, t),
    b: lerpChannel(from.b, to.b, t),
  };
}

function gradientT(u, v, gx, gy) {
  const dot = u * gx + v * gy;
  const mag = gx * gx + gy * gy;
  return Math.max(0, Math.min(1, dot / mag));
}

function pointInRoundedRect(x, y, width, height, radius) {
  const innerX = Math.max(radius, Math.min(width - radius, x));
  const innerY = Math.max(radius, Math.min(height - radius, y));
  const dx = x - innerX;
  const dy = y - innerY;
  return dx * dx + dy * dy <= radius * radius;
}

function pointInPolygon(x, y, polygon) {
  let inside = false;
  for (let i = 0, j = polygon.length - 1; i < polygon.length; j = i++) {
    const xi = polygon[i][0];
    const yi = polygon[i][1];
    const xj = polygon[j][0];
    const yj = polygon[j][1];
    const intersect =
      yi > y !== yj > y &&
      x < ((xj - xi) * (y - yi)) / (yj - yi + Number.EPSILON) + xi;
    if (intersect) inside = !inside;
  }
  return inside;
}

function buildScaledHexPoints(cx, cy, radius, gapScale, scale, offsetX, offsetY) {
  const size = radius * gapScale;
  return [
    [offsetX + scale * (cx + 0), offsetY + scale * (cy - size)],
    [offsetX + scale * (cx + SQ * size), offsetY + scale * (cy - 0.5 * size)],
    [offsetX + scale * (cx + SQ * size), offsetY + scale * (cy + 0.5 * size)],
    [offsetX + scale * (cx + 0), offsetY + scale * (cy + size)],
    [offsetX + scale * (cx - SQ * size), offsetY + scale * (cy + 0.5 * size)],
    [offsetX + scale * (cx - SQ * size), offsetY + scale * (cy - 0.5 * size)],
  ];
}

function writePpm(pathname, width, height, pixels) {
  const header = Buffer.from(`P6\n${width} ${height}\n255\n`, "ascii");
  writeFileSync(pathname, Buffer.concat([header, pixels]));
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

function writeRgbaPng(pathname, width, height, pixels) {
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  ihdr[10] = 0;
  ihdr[11] = 0;
  ihdr[12] = 0;

  const stride = width * 4;
  const raw = Buffer.alloc(height * (stride + 1));
  for (let y = 0; y < height; y += 1) {
    const rowOffset = y * (stride + 1);
    raw[rowOffset] = 0;
    pixels.copy(raw, rowOffset + 1, y * stride, (y + 1) * stride);
  }

  const idat = zlib.deflateSync(raw);
  const png = Buffer.concat([
    signature,
    pngChunk("IHDR", ihdr),
    pngChunk("IDAT", idat),
    pngChunk("IEND", Buffer.alloc(0)),
  ]);
  writeFileSync(pathname, png);
}

// macOS icon-grid: the rounded square occupies ~80% of the canvas (824/1024),
// leaving a transparent margin so it matches the sizing of other dock icons.
const ICON_PAD_RATIO = 100 / 1024; // ≈ 0.0977 margin on each side
const ICON_CORNER_RATIO = 0.2237; // continuous-corner radius / tile side

// Render the app icon directly to an RGBA PNG. Rendering here (rather than
// rasterizing the SVG through Quick Look) keeps the corners outside the
// squircle genuinely transparent — Quick Look flattens them to white.
function renderIconPng(pathname, size = 1024) {
  const brand = palette.studio;
  const tileTop = parseHexColor(brand.tileTop);
  const tileBottom = parseHexColor(brand.tileBottom);
  const warmTop = parseHexColor(brand.activeLight);
  const warmBottom = parseHexColor(brand.activeDark);
  const cellLight = parseHexColor(brand.cellLight);
  const cellMid = parseHexColor(brand.cellMid);

  const pad = size * ICON_PAD_RATIO;
  const tile = size - 2 * pad;
  const radius = tile * ICON_CORNER_RATIO;
  const scale = (tile / 92) * 0.6;
  const offsetX = size / 2;
  const offsetY = size / 2;
  const polygons = cells(14).map((cell) => {
    const points = buildScaledHexPoints(cell.cx, cell.cy, 14, 0.9, scale, offsetX, offsetY);
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

  const pixels = Buffer.alloc(size * size * 4, 0); // transparent by default

  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      const px = x + 0.5;
      const py = y + 0.5;

      // Outside the rounded-square tile stays transparent.
      if (!pointInRoundedRect(px - pad, py - pad, tile, tile, radius)) continue;

      let color = lerpColor(tileTop, tileBottom, gradientT(px / size, py / size, 0.5, 1));
      for (const polygon of polygons) {
        if (!pointInPolygon(px, py, polygon.points)) continue;
        if (polygon.active) {
          const u = (px - polygon.minX) / Math.max(1, polygon.maxX - polygon.minX);
          const v = (py - polygon.minY) / Math.max(1, polygon.maxY - polygon.minY);
          color = lerpColor(warmTop, warmBottom, gradientT(u, v, 0.4, 1));
        } else {
          color = polygon.ord % 2 ? cellMid : cellLight;
        }
      }

      const offset = (y * size + x) * 4;
      pixels[offset] = color.r;
      pixels[offset + 1] = color.g;
      pixels[offset + 2] = color.b;
      pixels[offset + 3] = 255;
    }
  }

  writeRgbaPng(pathname, size, size, pixels);
}

// The full 7-cell rosette is illegible at menu-bar size (~18pt), so the tray
// uses a single bold comb cell: a pointy-top hexagon ring. Outer hex fills
// ~82% of the frame height; the punched-out inner hex leaves a clean ring.
const TRAY_OUTER = 0.41; // outer hex radius as a fraction of canvas size
const TRAY_INNER_RATIO = 0.52; // inner (hole) hex radius / outer radius

function renderTrayTemplatePng(pathname, size = 128) {
  const cx = size / 2;
  const cy = size / 2;
  const outer = hexPolygon(cx, cy, size * TRAY_OUTER);
  const inner = hexPolygon(cx, cy, size * TRAY_OUTER * TRAY_INNER_RATIO);
  const pixels = Buffer.alloc(size * size * 4, 0);
  const samples = [
    [0.25, 0.25],
    [0.75, 0.25],
    [0.25, 0.75],
    [0.75, 0.75],
  ];

  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      let hits = 0;
      for (const [sx, sy] of samples) {
        const px = x + sx;
        const py = y + sy;
        // In the ring: inside the outer hex but not the inner hole.
        if (pointInPolygon(px, py, outer) && !pointInPolygon(px, py, inner)) {
          hits += 1;
        }
      }
      if (!hits) continue;
      const offset = (y * size + x) * 4;
      pixels[offset] = 0;
      pixels[offset + 1] = 0;
      pixels[offset + 2] = 0;
      pixels[offset + 3] = Math.round((255 * hits) / samples.length);
    }
  }

  writeRgbaPng(pathname, size, size, pixels);
}

function markGroup({
  markMode = "color",
  id = "hive-brand",
}) {
  const brand = palette.studio;
  const radius = 14;
  const paths = cells(radius)
    .map((cell) => {
      let fill;
      if (markMode === "mono") {
        fill = brand.ink;
      } else if (markMode === "template") {
        fill = "#000000";
      } else if (markMode === "knockout") {
        fill = "#ffffff";
      } else if (cell.active) {
        fill = `url(#warm-${id})`;
      } else {
        fill = cell.ord % 2 ? brand.cellMid : brand.cellLight;
      }
      return `<path d="${hex(cell.cx, cell.cy, radius)}" fill="${fill}"/>`;
    })
    .join("");

  return {
    defs: `
      <linearGradient id="cool-${id}" x1="0" y1="0" x2="0.4" y2="1">
        <stop offset="0" stop-color="${brand.honeyLight}"/>
        <stop offset="1" stop-color="${brand.honeyDark}"/>
      </linearGradient>
      <linearGradient id="warm-${id}" x1="0" y1="0" x2="0.4" y2="1">
        <stop offset="0" stop-color="${brand.activeLight}"/>
        <stop offset="1" stop-color="${brand.activeDark}"/>
      </linearGradient>
    `,
    paths,
  };
}

function buildMarkSvg({ size = 512, markMode = "color", id = "mark" } = {}) {
  const { defs, paths } = markGroup({ markMode, id });
  return `<svg width="${size}" height="${size}" viewBox="-46 -46 92 92" fill="none" xmlns="http://www.w3.org/2000/svg">
  <title>Hive Brand Mark</title>
  <defs>${defs}</defs>
  <g>${paths}</g>
</svg>
`;
}

function buildIconSvg({ size = 1024, markMode = "color", id = "icon" } = {}) {
  const brand = palette.studio;
  // Inset the rounded-square into the macOS icon grid (transparent margin).
  const pad = size * ICON_PAD_RATIO;
  const tile = size - 2 * pad;
  const corner = tile * ICON_CORNER_RATIO;
  const scale = (tile / 92) * 0.6;
  const { defs, paths } = markGroup({ markMode, id });

  return `<svg width="${size}" height="${size}" viewBox="0 0 ${size} ${size}" fill="none" xmlns="http://www.w3.org/2000/svg">
  <title>Hive App Icon</title>
  <defs>
    ${defs}
    <linearGradient id="tile-${id}" x1="0" y1="0" x2="0.5" y2="1">
      <stop offset="0" stop-color="${brand.tileTop}"/>
      <stop offset="1" stop-color="${brand.tileBottom}"/>
    </linearGradient>
  </defs>
  <rect x="${pad}" y="${pad}" width="${tile}" height="${tile}" rx="${corner}" ry="${corner}" fill="${markMode === "knockout" ? "none" : `url(#tile-${id})`}" />
  <g transform="translate(${size / 2},${size / 2}) scale(${scale})">
    ${paths}
  </g>
</svg>
`;
}

function buildTraySvg({ size = 64 } = {}) {
  const cx = size / 2;
  const cy = size / 2;
  // A single comb cell (hexagon ring) — legible at menu-bar size, where the
  // full rosette turns to noise. evenodd punches the inner hex into a ring.
  const ring =
    hexPathString(cx, cy, size * TRAY_OUTER) +
    " " +
    hexPathString(cx, cy, size * TRAY_OUTER * TRAY_INNER_RATIO);
  return `<svg width="${size}" height="${size}" viewBox="0 0 ${size} ${size}" fill="none" xmlns="http://www.w3.org/2000/svg">
  <title>Hive Tray Template</title>
  <path d="${ring}" fill="#000000" fill-rule="evenodd"/>
</svg>
`;
}

function buildLockupSvg({ id = "lockup" } = {}) {
  const mark = buildMarkSvg({ size: 120, id: `${id}-mark` });
  return `<svg width="720" height="220" viewBox="0 0 720 220" fill="none" xmlns="http://www.w3.org/2000/svg">
  <title>Hive Documentation Logo</title>
  <defs>
    <clipPath id="clip-${id}">
      <rect x="36" y="52" width="76" height="76" rx="0" ry="0"/>
    </clipPath>
  </defs>
  <g transform="translate(36 52)">
    ${mark.replace("<svg width=\"120\" height=\"120\" viewBox=\"-46 -46 92 92\" fill=\"none\" xmlns=\"http://www.w3.org/2000/svg\">", "").replace("</svg>\n", "")}
  </g>
  <text x="146" y="124" fill="${palette.studio.ink}" font-family="Inter, &quot;SF Pro Display&quot;, system-ui, sans-serif" font-size="84" font-weight="700" letter-spacing="-3">hive</text>
</svg>
`;
}

function writeFile(name, contents) {
  writeFileSync(path.join(__dirname, name), contents, "utf8");
}

const pngIndex = process.argv.indexOf("--png");
if (pngIndex !== -1) {
  const pngPath = process.argv[pngIndex + 1];
  if (!pngPath) {
    throw new Error("missing path for --png");
  }
  const sizeArg = Number(process.argv[pngIndex + 2]);
  renderIconPng(pngPath, Number.isFinite(sizeArg) && sizeArg > 0 ? sizeArg : 1024);
}

mkdirSync(__dirname, { recursive: true });

writeFile("hive-mark.svg", buildMarkSvg({ size: 512, id: "asset-mark" }));
writeFile("hive-mark-mono.svg", buildMarkSvg({ size: 512, markMode: "mono", id: "asset-mark-mono" }));
writeFile("hive-app-icon.svg", buildIconSvg({ size: 1024, id: "asset-icon" }));
writeFile("hive-tray-template.svg", buildTraySvg({ size: 64, id: "asset-tray-template" }));
writeFile("hive-lockup.svg", buildLockupSvg({ id: "asset-lockup" }));
writeFile("hive-brand-tokens.json", JSON.stringify(palette, null, 2) + "\n");
renderTrayTemplatePng(path.join(__dirname, "hive-tray-template-128.png"));

console.log("Generated brand assets in", __dirname);
