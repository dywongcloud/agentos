"use client";

// Deterministic 16x16 pixel-art portrait, seeded by agent id+name (no assets,
// no network). Layered like a character generator — hairstyle (crop, long,
// afro, bob, headwrap, beanie, bun, side-pony), clothing (crew, hoodie,
// collar, stripe), facial features (brows, pupils, blush, glasses) and
// per-color shading — so every agent gets a distinct, detailed little
// character in the relevance.ai / relay.app style.

import type { ReactElement } from "react";

function hashStr(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

// mulberry32 — independent picks per layer instead of bit-slicing one hash.
function rng(seed: number): () => number {
  let a = seed | 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function darken(hex: string, f: number): string {
  const n = parseInt(hex.slice(1), 16);
  const r = Math.round(((n >> 16) & 255) * f);
  const g = Math.round(((n >> 8) & 255) * f);
  const b = Math.round((n & 255) * f);
  return `#${((r << 16) | (g << 8) | b).toString(16).padStart(6, "0")}`;
}

const SKIN = ["#ffe0bd", "#f5cda2", "#eab98a", "#d29b6c", "#a9714b", "#7c4f2c"];
const HAIR = [
  "#23232e", "#4a3123", "#7c4a21", "#b5742c", "#e3b04b", "#d8d8e0",
  "#8e44ad", "#5b6ee1", "#3aa6a6", "#e15b97", "#27ae60", "#c1440e",
];
const SHIRT = [
  "#5b5bd6", "#2a9d8f", "#e76f51", "#457b9d", "#b5179e",
  "#588157", "#bc6c25", "#9d4edd", "#1d7874", "#d62828",
];
const ACCENT = ["#f4a261", "#e9c46a", "#e76f51", "#9d4edd", "#43aa8b", "#f72585", "#4895ef"];
const BG = ["#dbeafe", "#e0e7ff", "#fce7f3", "#dcfce7", "#fef3c7", "#e0f2fe", "#f3e8ff", "#ffedd5"];

// Grid chars: . none | S skin | s skin shade | H hair | h hair shade |
// C accent | c accent shade | T shirt | t shirt shade | W white |
// E eye white | P pupil | B brow | M mouth | R blush | G glasses

const BASE = [
  "................",
  "................",
  "................",
  "....SSSSSSSS....",
  "...SSSSSSSSSS...",
  "...SSSSSSSSSS...",
  "...SSSSSSSSSS...",
  "...SSSSSSSSSS...",
  "...SSSSSSSSSS...",
  "...SSSSSSSSSS...",
  "....SSSSSSSS....",
  "....sSSSSSSs....",
  "......SSSS......",
  "................",
  "................",
  "................",
];

const HAIRSTYLES: string[][] = [
  // short crop
  [
    "................",
    "................",
    "....HHHHHHHH....",
    "...HHHHHHHHHH...",
    "...HHhHHHHhHH...",
    "...HH......HH...",
    "...H........H...",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
  // long hair over the shoulders
  [
    "................",
    "................",
    "....HHHHHHHH....",
    "...HHHHHHHHHH...",
    "..HHHhHHHHhHHH..",
    "..HHH......HHH..",
    "..HH........HH..",
    "..HH........HH..",
    "..HH........HH..",
    "..HH........HH..",
    "..HHh......hHH..",
    "..HH........HH..",
    "..hh........hh..",
    "................",
    "................",
    "................",
  ],
  // afro / curly
  [
    "................",
    "...HHHHHHHHHH...",
    "..HHHHHHHHHHHH..",
    "..HHHHHHHHHHHH..",
    "..HHhHHHHHHhHH..",
    "..HHH......HHH..",
    "..HH........HH..",
    "...H........H...",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
  // bob with full fringe
  [
    "................",
    "................",
    "....HHHHHHHH....",
    "...HHHHHHHHHH...",
    "...HHHHHHHHHH...",
    "...HHhHHHHhHH...",
    "...HH......HH...",
    "...HH......HH...",
    "...HH......HH...",
    "...Hh......hH...",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
  // headwrap with top knot (accent color)
  [
    "................",
    ".....CC.........",
    "....CCCCCCCC....",
    "...CCCCCCCCCC...",
    "...CCcCCCCcCC...",
    "...cC......Cc...",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
  // beanie with band, hair peeking out
  [
    "................",
    "....CCCCCCCC....",
    "....CCCCCCCC....",
    "...CCCCCCCCCC...",
    "...cccccccccc...",
    "...HH......HH...",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
  // top bun
  [
    ".......HH.......",
    "......HHHH......",
    "....HHHHHHHH....",
    "...HHHHHHHHHH...",
    "...HHhHHHHhHH...",
    "...HH......HH...",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
  // side ponytail
  [
    "................",
    "................",
    "....HHHHHHHH....",
    "...HHHHHHHHHHH..",
    "...HHhHHHHhHHH..",
    "...HH......HHH..",
    "...H........HH..",
    ".............HH.",
    ".............HH.",
    ".............hH.",
    "..............H.",
    "................",
    "................",
    "................",
    "................",
    "................",
  ],
];

const CLOTHING: string[][] = [
  // crew neck
  [
    "...TTTTttTTTT...",
    "..TTTTTTTTTTTT..",
    "..TTTTTTTTTTTT..",
  ],
  // hoodie with drawstrings (accent)
  [
    "...TTTTttTTTT...",
    "..TTTCTTTTCTTT..",
    "..TTTCTTTTCTTT..",
  ],
  // collared shirt
  [
    "...TTTWttWTTT...",
    "..TTTTTWWTTTTT..",
    "..TTTTTTTTTTTT..",
  ],
  // stripe tee (accent stripe)
  [
    "...TTTTttTTTT...",
    "..TTTTTTTTTTTT..",
    "..CCCCCCCCCCCC..",
  ],
];

function stamp(grid: string[][], overlay: string[], rowOffset = 0) {
  overlay.forEach((row, y) => {
    const gy = y + rowOffset;
    if (gy < 0 || gy > 15) return;
    for (let x = 0; x < 16; x++) {
      const c = row[x]!;
      if (c !== ".") grid[gy]![x] = c;
    }
  });
}

export default function PixelAvatar({
  seed,
  size = 34,
}: {
  seed: string;
  size?: number;
}) {
  const r = rng(hashStr(seed));
  const pick = <T,>(arr: T[]): T => arr[Math.floor(r() * arr.length)]!;

  const skin = pick(SKIN);
  const hair = pick(HAIR);
  const shirt = pick(SHIRT);
  const accent = pick(ACCENT);
  const bg = pick(BG);
  const hairstyle = pick(HAIRSTYLES);
  const clothing = pick(CLOTHING);
  const hasBlush = r() < 0.45;
  const hasGlasses = r() < 0.25;

  const grid: string[][] = Array.from({ length: 16 }, () => Array(16).fill("."));
  stamp(grid, BASE);
  stamp(grid, hairstyle);
  stamp(grid, clothing, 13);

  // Face features go on top of hair so fringes never hide the eyes.
  grid[6]![5] = "B"; grid[6]![6] = "B"; grid[6]![9] = "B"; grid[6]![10] = "B";
  grid[7]![5] = "E"; grid[7]![6] = "P"; grid[7]![9] = "P"; grid[7]![10] = "E";
  grid[9]![7] = "M"; grid[9]![8] = "M";
  if (hasBlush) { grid[8]![4] = "R"; grid[8]![11] = "R"; }
  if (hasGlasses) {
    grid[7]![4] = "G"; grid[7]![7] = "G"; grid[7]![8] = "G"; grid[7]![11] = "G";
  }

  const color: Record<string, string> = {
    S: skin,
    s: darken(skin, 0.82),
    H: hair,
    h: darken(hair, 0.72),
    C: accent,
    c: darken(accent, 0.72),
    T: shirt,
    t: darken(shirt, 0.72),
    W: "#f5f5f7",
    E: "#ffffff",
    P: "#23232e",
    B: darken(hair, 0.6),
    M: "#b85c4a",
    R: "#e8927c",
    G: "#2b2f3a",
  };

  const cells: ReactElement[] = [];
  grid.forEach((row, y) => {
    row.forEach((c, x) => {
      const fill = color[c];
      if (fill) {
        cells.push(
          <rect key={`${x}-${y}`} x={x} y={y} width={1.06} height={1.06} fill={fill} />
        );
      }
    });
  });

  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      shapeRendering="crispEdges"
      style={{ borderRadius: 9, background: bg, flexShrink: 0 }}
    >
      {cells}
    </svg>
  );
}
