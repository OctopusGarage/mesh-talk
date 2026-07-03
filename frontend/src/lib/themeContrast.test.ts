// @ts-expect-error Vitest runs this test in Node; the app tsconfig intentionally omits Node types.
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const css = readFileSync(new URL("../index.css", import.meta.url), "utf8");
const MIN_TEXT_CONTRAST = 4.5;

function block(selector: string): string {
  const index = css.indexOf(selector);
  if (index < 0) throw new Error(`Missing CSS selector: ${selector}`);
  const start = css.indexOf("{", index) + 1;
  let depth = 1;
  let cursor = start;
  for (; cursor < css.length; cursor++) {
    if (css[cursor] === "{") depth += 1;
    if (css[cursor] === "}") depth -= 1;
    if (depth === 0) break;
  }
  return css.slice(start, cursor);
}

function varsFrom(text: string): Record<string, string> {
  return Object.fromEntries(
    Array.from(text.matchAll(/--([a-z-]+):\s*([^;]+);/g), (match) => [
      match[1],
      match[2].replace(/\/\*.*?\*\//g, "").trim(),
    ]),
  );
}

const root = varsFrom(block(":root"));
const dark = { ...root, ...varsFrom(block(".dark")) };
const themes = {
  light: root,
  dark,
  oled: { ...dark, ...varsFrom(block(".oled")) },
  argentina: {
    ...root,
    ...varsFrom(block('html[data-palette="argentina"]')),
  },
  barcelona: {
    ...dark,
    ...varsFrom(block('html[data-palette="barcelona"]')),
  },
  messi: { ...root, ...varsFrom(block('html[data-palette="messi"]')) },
};

function parseHsl(value: string): [number, number, number] {
  const numbers = value.match(/-?\d+(?:\.\d+)?/g)?.map(Number);
  if (!numbers || numbers.length < 3) throw new Error(`Invalid HSL: ${value}`);
  return [numbers[0], numbers[1] / 100, numbers[2] / 100];
}

function hslToRgb([h, s, l]: [number, number, number]): [
  number,
  number,
  number,
] {
  h = (((h % 360) + 360) % 360) / 360;
  if (s === 0) return [l, l, l];

  const hueToRgb = (p: number, q: number, t: number) => {
    if (t < 0) t += 1;
    if (t > 1) t -= 1;
    if (t < 1 / 6) return p + (q - p) * 6 * t;
    if (t < 1 / 2) return q;
    if (t < 2 / 3) return p + (q - p) * (2 / 3 - t) * 6;
    return p;
  };

  const q = l < 0.5 ? l * (1 + s) : l + s - l * s;
  const p = 2 * l - q;
  return [
    hueToRgb(p, q, h + 1 / 3),
    hueToRgb(p, q, h),
    hueToRgb(p, q, h - 1 / 3),
  ];
}

function luminance(value: string): number {
  const [red, green, blue] = hslToRgb(parseHsl(value)).map((channel) =>
    channel <= 0.03928 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4,
  );
  return 0.2126 * red + 0.7152 * green + 0.0722 * blue;
}

function contrast(foreground: string, background: string): number {
  const fg = luminance(foreground);
  const bg = luminance(background);
  return (Math.max(fg, bg) + 0.05) / (Math.min(fg, bg) + 0.05);
}

describe("chat link theme contrast", () => {
  it("keeps sent-message text readable in every theme", () => {
    for (const [theme, vars] of Object.entries(themes)) {
      expect(
        contrast(vars["bubble-own-foreground"], vars["bubble-own"]),
        `${theme} sent text`,
      ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    }
  });

  it("keeps received-message links readable in every theme", () => {
    for (const [theme, vars] of Object.entries(themes)) {
      expect(
        contrast(vars["message-link"], vars.muted),
        `${theme} received link`,
      ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    }
  });

  it("keeps sent-message links readable in every theme", () => {
    for (const [theme, vars] of Object.entries(themes)) {
      expect(
        contrast(vars["bubble-own-link"], vars["bubble-own"]),
        `${theme} sent link`,
      ).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    }
  });
});
