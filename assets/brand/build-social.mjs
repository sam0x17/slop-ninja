import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const source = path.dirname(fileURLToPath(import.meta.url));
const output = path.resolve(process.argv[2] ?? path.join(os.homedir(), "Documents/SlopNinja"));
const svgOutput = path.join(output, "SVG");
const ink = "#0B1014";
const mint = "#28FABA";

function mark(file, x, y, width, height, opacity = 1) {
  const raw = fs.readFileSync(path.join(source, file), "utf8");
  const root = raw.match(/<svg\b([^>]*)>([\s\S]*?)<\/svg>\s*$/);
  const viewBox = root?.[1].match(/viewBox="([^"]+)"/)?.[1];
  if (!viewBox) throw new Error(`Missing viewBox: ${file}`);
  const body = root[2].replace(/<(title|desc)\b[^>]*>[\s\S]*?<\/\1>/g, "");
  return `<svg x="${x}" y="${y}" width="${width}" height="${height}" viewBox="${viewBox}" opacity="${opacity}">${body}</svg>`;
}

const icon = (x, y, size, opacity = 1) => mark("slop-ninja-icon.svg", x, y, size, size, opacity);
const wordmark = (x, y, width) => mark("slop-ninja-wordmark.svg", x, y, width, width * 208 / 952);

function canvas(width, height, title, body) {
  return `<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}" role="img">
  <title>${title}</title>
  <defs>
    <radialGradient id="social-glow">
      <stop offset="0" stop-color="${mint}" stop-opacity="0.12"/>
      <stop offset="1" stop-color="${mint}" stop-opacity="0"/>
    </radialGradient>
  </defs>
  <rect width="${width}" height="${height}" fill="${ink}"/>
  ${body}
</svg>`;
}

function backdrop() {
  return `<ellipse cx="1240" cy="225" rx="540" ry="400" fill="url(#social-glow)"/>
  <path d="M1225 0H1295L975 500H905Z" fill="${mint}" opacity="0.018"/>
  <path d="M1430 0H1450L1130 500H1110Z" fill="${mint}" opacity="0.035"/>`;
}

const assets = [
  {
    name: "twitter-avatar", width: 1024, height: 1024,
    svg: canvas(1024, 1024, "Slop Ninja profile avatar", `
      <ellipse cx="540" cy="490" rx="460" ry="460" fill="url(#social-glow)"/>
      ${icon(62, 48, 900)}`),
  },
  {
    name: "twitter-header", width: 1500, height: 500,
    svg: canvas(1500, 500, "Slop Ninja: Detect slop. Fix it.", `
      ${backdrop()}
      ${wordmark(380, 130, 650)}
      <text x="394" y="327" fill="#FCFCFD" font-family="Avenir Next, Arial, sans-serif" font-size="38" font-weight="500" letter-spacing="0.2">Detect slop. Fix it.</text>
      ${icon(1095, 65, 350)}`),
  },
  {
    name: "twitter-header-minimal", width: 1500, height: 500,
    svg: canvas(1500, 500, "Slop Ninja wordmark header", `
      ${backdrop()}
      ${wordmark(575, 164, 730)}`),
  },
  {
    name: "twitter-header-artwork", width: 1500, height: 500,
    svg: canvas(1500, 500, "Slop Ninja illustrated header", `
      ${backdrop()}
      ${icon(980, 10, 480)}`),
  },
];

fs.mkdirSync(svgOutput, { recursive: true });
for (const asset of assets) {
  const svgFile = path.join(svgOutput, `${asset.name}.svg`);
  const pngFile = path.join(output, `${asset.name}.png`);
  fs.writeFileSync(svgFile, asset.svg);
  execFileSync("rsvg-convert", ["--format", "png", "--width", String(asset.width), "--height", String(asset.height), "--output", pngFile, svgFile]);
  console.log(pngFile);
}

fs.writeFileSync(path.join(output, "README.txt"), `Slop Ninja social assets

twitter-avatar.png          1024 x 1024, composed for a circular profile crop
twitter-header.png          1500 x 500, wordmark, tagline and ninja illustration
twitter-header-minimal.png  1500 x 500, wordmark only
twitter-header-artwork.png  1500 x 500, illustration without text

The PNGs are ready to upload to Twitter/X. Choose one header.
Important header artwork stays away from the lower-left avatar overlap.
Every background is dark. The approved logo paths are preserved.
SVG/ contains editable, self-contained vector compositions. The tagline uses
Avenir Next, with Arial and sans-serif fallbacks; PNG text is already rendered.

Rebuild from the repository:
  node assets/brand/build-social.mjs "${output}"
Requires Node.js and rsvg-convert (librsvg).
`);
