import { cn } from "@/lib/utils";

// One postal family, four silhouettes. Broad strokes remain legible at 28px;
// open lower-right corners leave room for the caller's presence indicator.
const EMBLEMS = [
  {
    name: "dispatch",
    paths: ["M15 21h34v23H15z", "m15 22 17 13 17-13M15 44l11-12m23 12L38 32"],
  },
  {
    name: "postbox",
    paths: ["M19 47V29a13 13 0 0 1 26 0v18H19Z", "M25 29h14M26 38h12M32 16v6"],
  },
  {
    name: "inbox",
    paths: ["M16 34h9l4 5h6l4-5h9v13H16Z", "M22 34V18h20v16M27 24h10M27 29h10"],
  },
  {
    name: "relay",
    paths: [
      "M20 24h24v18H20zM20 25l12 9 12-9",
      "M12 18h15M37 48h15M12 18v8m40 22v-8",
    ],
  },
] as const;

export function PostOfficeGlyph({
  variant,
  size,
  title,
  verified,
  className,
}: {
  variant: number;
  size: number;
  title?: string;
  verified?: boolean;
  className?: string;
}) {
  const emblem = EMBLEMS[variant % EMBLEMS.length];
  return (
    <svg
      viewBox="0 0 64 64"
      width={size}
      height={size}
      role="img"
      aria-label={title ?? "Post office"}
      data-post-office-avatar={emblem.name}
      className={cn(
        "shrink-0 rounded-[28%] text-signal",
        verified && "ring-2 ring-verified",
        className,
      )}
    >
      {title && <title>{title}</title>}
      <rect width="64" height="64" rx="18" fill="hsl(var(--card))" />
      <rect
        width="64"
        height="64"
        rx="18"
        fill="currentColor"
        fillOpacity=".1"
      />
      <g
        fill="none"
        stroke="currentColor"
        strokeWidth="3"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {emblem.paths.map((d) => (
          <path key={d} d={d} />
        ))}
      </g>
    </svg>
  );
}
