// Vitest runs in Node. The app tsconfig only includes the DOM types, so the
// builtins are suppressed here rather than added to the browser project.
// @ts-expect-error node builtin, available under Vitest
import { readFileSync } from "node:fs";
// @ts-expect-error node builtin, available under Vitest
import { dirname, join } from "node:path";
// @ts-expect-error node builtin, available under Vitest
import { fileURLToPath } from "node:url";
import { expect, it } from "vitest";

const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../styles.css"), "utf8");

/** Text on its background. WCAG AA for 13 px text is 4.5. */
const TEXT_PAIRS = [
  ["foreground", "background"],
  ["muted-foreground", "background"],
  ["card-foreground", "card"],
  ["popover-foreground", "popover"],
  ["primary-foreground", "primary"],
  ["secondary-foreground", "secondary"],
  ["accent-foreground", "accent"],
  ["sidebar-foreground", "sidebar"],
  ["sidebar-primary-foreground", "sidebar-primary"],
  ["sidebar-accent-foreground", "sidebar-accent"],
  ["destructive", "background"],
] as const;

/** Status dots are not text. WCAG 1.4.11 asks 3:1 for a graphical object. */
const DOT_PAIRS = [
  ["status-ok", "background"],
  ["status-error", "background"],
  ["status-unset", "background"],
] as const;

it("keeps text tokens at WCAG AA in light and dark", () => {
  for (const theme of ["light", "dark"] as const) {
    const tokens = themeTokens(css, theme);
    for (const [front, back] of TEXT_PAIRS) {
      expect(contrast(tokens, front, back), `${theme} ${front} on ${back}`).toBeGreaterThanOrEqual(
        4.5,
      );
    }
    for (const [front, back] of DOT_PAIRS) {
      expect(contrast(tokens, front, back), `${theme} ${front} on ${back}`).toBeGreaterThanOrEqual(
        3,
      );
    }
  }
});

function themeTokens(source: string, theme: "light" | "dark"): Map<string, Oklch> {
  const header = theme === "light" ? ":root" : ".dark";
  const start = source.indexOf(`${header} {`);
  const open = source.indexOf("{", start);
  const close = source.indexOf("}", open);
  const body = source.slice(open + 1, close);
  const tokens = new Map<string, Oklch>();
  for (const match of body.matchAll(
    /--([a-z0-9-]+):\s*oklch\(\s*([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*\)/g,
  )) {
    const name = match[1];
    const l = match[2];
    const c = match[3];
    const h = match[4];
    if (!name || l === undefined || c === undefined || h === undefined) {
      continue;
    }
    tokens.set(name, [Number(l), Number(c), Number(h)]);
  }
  return tokens;
}

type Oklch = [number, number, number];

function contrast(tokens: Map<string, Oklch>, front: string, back: string): number {
  const fg = tokens.get(front);
  const bg = tokens.get(back);
  if (!fg || !bg) {
    throw new Error(`missing ${front} or ${back}`);
  }
  const lighter = Math.max(luminance(fg), luminance(bg));
  const darker = Math.min(luminance(fg), luminance(bg));
  return (lighter + 0.05) / (darker + 0.05);
}

/** OKLCH to WCAG relative luminance. The OKLab matrix already yields linear RGB. */
function luminance([l, c, h]: Oklch): number {
  const rad = (h * Math.PI) / 180;
  const a = c * Math.cos(rad);
  const b = c * Math.sin(rad);
  const lCube = (l + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const mCube = (l - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const sCube = (l - 0.0894841775 * a - 1.291485548 * b) ** 3;
  const red = 4.0767416621 * lCube - 3.3077115913 * mCube + 0.2309699292 * sCube;
  const green = -1.2684380046 * lCube + 2.6097574011 * mCube - 0.3413193965 * sCube;
  const blue = -0.0041960863 * lCube - 0.7034186147 * mCube + 1.707614701 * sCube;
  const channel = (value: number) => Math.min(1, Math.max(0, value));
  return 0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue);
}
