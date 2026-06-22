import { useId } from "react";
import { cn } from "@/lib/utils";

/**
 * Logo — the Mesh-Talk BRAND mark (distinct from the per-user `IdentityGlyph`, which is
 * the user/contact avatar). "Concept 3": a circuit-trace "M" monogram with a teal→green
 * gradient, node pads at every junction, a faint blueprint mesh grid, and a soft glow.
 *
 * In-app the glyph renders WITHOUT the opaque dark tile so it sits on the app's own dark
 * surface (`tile` defaults to false); pass `tile` to draw the rounded ink tile (used for
 * the app icon export). Reduced-motion safe — purely static, nothing spins.
 */
export interface LogoProps {
  /** Rendered size in px (square). Default 40. */
  size?: number;
  /** Draw the rounded dark "ink" tile + border behind the glyph. Default false. */
  tile?: boolean;
  className?: string;
  title?: string;
}

export function Logo({
  size = 40,
  tile = false,
  className,
  title = "Mesh-Talk",
}: LogoProps) {
  // Unique gradient ids so multiple <Logo>s on a page don't collide.
  const uid = useId().replace(/:/g, "");
  const trace = `mt-trace-${uid}`;
  const glow = `mt-glow-${uid}`;

  return (
    <svg
      viewBox="0 0 512 512"
      width={size}
      height={size}
      role="img"
      aria-label={title}
      className={cn("shrink-0", className)}
    >
      <title>{title}</title>
      <defs>
        <linearGradient id={trace} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#34D399" />
          <stop offset="0.55" stopColor="#2DD4BF" />
          <stop offset="1" stopColor="#15B8A6" />
        </linearGradient>
        <radialGradient id={glow} cx="0.5" cy="0.5" r="0.6">
          <stop offset="0" stopColor="#2DD4BF" stopOpacity="0.26" />
          <stop offset="1" stopColor="#2DD4BF" stopOpacity="0" />
        </radialGradient>
      </defs>

      {/* Ink tile — optional. Off for in-app use (sits on the app's dark surface),
          on for the app-icon export. */}
      {tile && (
        <>
          <rect width="512" height="512" rx="116" fill="#0B0F12" />
          <rect
            x="1.5"
            y="1.5"
            width="509"
            height="509"
            rx="114.5"
            fill="none"
            stroke="#1B2227"
            strokeWidth="3"
          />
        </>
      )}

      {/* Soft teal glow — kept in both modes so the mark always feels lit. */}
      <circle cx="256" cy="256" r="190" fill={`url(#${glow})`} />

      {/* Faint underlying mesh grid (the 'mesh' substrate). */}
      <g stroke="#16C0AE" strokeOpacity="0.12" strokeWidth="2">
        <line x1="128" y1="110" x2="128" y2="402" />
        <line x1="256" y1="110" x2="256" y2="402" />
        <line x1="384" y1="110" x2="384" y2="402" />
        <line x1="110" y1="170" x2="402" y2="170" />
        <line x1="110" y1="340" x2="402" y2="340" />
      </g>

      {/* The M trace: left leg up, chamfer to center valley, up to right peak,
          down the right leg. */}
      <path
        d="M132 372 L132 168 L168 132 L256 240 L344 132 L380 168 L380 372"
        fill="none"
        stroke={`url(#${trace})`}
        strokeWidth="26"
        strokeLinecap="round"
        strokeLinejoin="round"
      />

      {/* Circuit branch stubs off the legs (tech-feel detail). */}
      <g stroke={`url(#${trace})`} strokeWidth="12" strokeLinecap="round">
        <line x1="132" y1="270" x2="92" y2="270" />
        <line x1="380" y1="270" x2="420" y2="270" />
      </g>

      {/* Node pads. The terminal/center pads carry a dark fill so they read as drilled
          PCB vias even when the tile is off. */}
      <g>
        <circle
          cx="132"
          cy="372"
          r="22"
          fill="#0B0F12"
          stroke={`url(#${trace})`}
          strokeWidth="13"
        />
        <circle
          cx="380"
          cy="372"
          r="22"
          fill="#0B0F12"
          stroke={`url(#${trace})`}
          strokeWidth="13"
        />
        <circle cx="92" cy="270" r="13" fill="#2DD4BF" />
        <circle cx="420" cy="270" r="13" fill="#2DD4BF" />
        <circle cx="150" cy="150" r="15" fill="#34D399" />
        <circle cx="362" cy="150" r="15" fill="#34D399" />
        <circle
          cx="256"
          cy="240"
          r="26"
          fill="#0B0F12"
          stroke={`url(#${trace})`}
          strokeWidth="13"
        />
        <circle cx="256" cy="240" r="9" fill="#34D399" />
      </g>
    </svg>
  );
}
