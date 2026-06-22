import { useMemo } from "react";
import { cn } from "@/lib/utils";
import { useAvatar } from "@/store/avatars";

/**
 * IdentityGlyph — a deterministic, on-brand SVG sigil derived from a fingerprint /
 * user id. Same seed → same glyph (stable identity across renders & sessions).
 *
 * The glyph is a symmetric "mesh-node" lattice: a small grid of nodes with edges
 * between active cells, mirrored horizontally so it reads as a crest rather than
 * random noise. Color is a single hue derived from the seed but harmonized toward the
 * teal/ink palette (constrained saturation/lightness) so it always feels on-brand —
 * never garish.
 */

/** FNV-1a 32-bit hash → stable, well-distributed unsigned int. */
function hashSeed(seed: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < seed.length; i++) {
    h ^= seed.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/** A tiny seeded PRNG (mulberry32) for deriving deterministic cells. */
function mulberry32(a: number): () => number {
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const GRID = 5; // 5x5 lattice, horizontally symmetric (3 unique columns)
const CELL = 7;
const PAD = 6;
const VIEW = GRID * CELL + PAD * 2;

export interface IdentityGlyphProps {
  /** The id / fingerprint to render. Same seed → same glyph. */
  seed: string;
  /** Rendered size in px (square). Default 40. */
  size?: number;
  /** Optional verified state — adds a subtle teal/verified ring. */
  verified?: boolean;
  className?: string;
  title?: string;
}

export function IdentityGlyph({
  seed,
  size = 40,
  verified = false,
  className,
  title,
}: IdentityGlyphProps) {
  // Central avatar resolution: every personal-identity spot routes its id through
  // IdentityGlyph's `seed`, so resolving a custom avatar here makes a user's chosen photo
  // appear EVERYWHERE that identity renders (sidebar, bubbles, dialogs…) — and clearing
  // it transparently falls back to the deterministic glyph below. `useAvatar` returns a
  // stable string|undefined, so this hook adds no re-render churn.
  const customAvatar = useAvatar(seed);

  const { cells, hue, accentHue, rng } = useMemo(() => {
    const h = hashSeed(seed || "·");
    const rand = mulberry32(h);
    // Hue harmonized toward teal/ink: bias into the 150–215 (teal→blue) arc.
    const hue = 150 + Math.floor(rand() * 65);
    const accentHue = (hue + 28) % 360;
    // Build a horizontally-mirrored grid of active cells.
    const cols = Math.ceil(GRID / 2); // 3 unique columns
    const cells: boolean[][] = Array.from({ length: GRID }, () =>
      Array.from({ length: GRID }, () => false),
    );
    for (let y = 0; y < GRID; y++) {
      for (let x = 0; x < cols; x++) {
        const on = rand() > 0.45;
        cells[y][x] = on;
        cells[y][GRID - 1 - x] = on;
      }
    }
    return { cells, hue, accentHue, rng: rand };
  }, [seed]);

  // A custom avatar replaces the deterministic glyph. Same square footprint + rounded
  // corners as the SVG so presence-dot / verified overlays positioned by callers still
  // sit correctly; the verified ring is mirrored as a border so the badge reads the same.
  if (customAvatar) {
    return (
      <img
        src={customAvatar}
        alt={title ?? "avatar"}
        title={title}
        width={size}
        height={size}
        className={cn(
          "shrink-0 rounded-[28%] object-cover",
          verified && "ring-2 ring-verified",
          className,
        )}
      />
    );
  }

  const fg = `hsl(${hue} 58% 56%)`;
  const fgDim = `hsl(${hue} 42% 46%)`;
  const accent = `hsl(${accentHue} 62% 60%)`;
  const ringColor = verified
    ? "hsl(var(--verified))"
    : `hsl(${hue} 40% 50% / 0.45)`;

  return (
    <svg
      viewBox={`0 0 ${VIEW} ${VIEW}`}
      width={size}
      height={size}
      role="img"
      aria-label={title ?? "identity glyph"}
      className={cn("shrink-0 rounded-[28%]", className)}
    >
      {title && <title>{title}</title>}
      <rect
        width={VIEW}
        height={VIEW}
        rx={VIEW * 0.28}
        fill="hsl(var(--card))"
        stroke={ringColor}
        strokeWidth={verified ? 1.6 : 1}
      />
      <g>
        {cells.flatMap((row, y) =>
          row.map((on, x) => {
            if (!on) return null;
            const cx = PAD + x * CELL + CELL / 2;
            const cy = PAD + y * CELL + CELL / 2;
            // Center column nodes get the accent tint for a focal "core".
            const isCore = x === Math.floor(GRID / 2);
            return (
              <circle
                key={`${x}-${y}`}
                cx={cx}
                cy={cy}
                r={CELL * (isCore ? 0.34 : 0.28)}
                fill={isCore ? accent : rng() > 0.5 ? fg : fgDim}
              />
            );
          }),
        )}
        {/* Vertical spine connecting the symmetric core for a "mesh-node" read. */}
        <line
          x1={VIEW / 2}
          y1={PAD + CELL / 2}
          x2={VIEW / 2}
          y2={VIEW - PAD - CELL / 2}
          stroke={accent}
          strokeWidth={0.8}
          strokeOpacity={0.5}
        />
      </g>
    </svg>
  );
}
