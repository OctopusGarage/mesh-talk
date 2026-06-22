import { cn } from "@/lib/utils";

export type PresenceStatus = "online" | "away" | "recent" | "offline";

const COLOR: Record<PresenceStatus, string> = {
  online: "hsl(var(--signal))",
  away: "hsl(var(--mention))",
  recent: "hsl(var(--mention))",
  offline: "hsl(var(--muted-foreground))",
};

const SIZE = { sm: 8, md: 10, lg: 12 } as const;

export interface PresenceDotProps {
  status?: PresenceStatus;
  size?: keyof typeof SIZE;
  className?: string;
  /** Accessible label; defaults to the status word. */
  label?: string;
}

/**
 * PresenceDot — a small status dot with a soft "breathing" pulse when online (a living
 * LAN-presence signal). Steady dim when offline; the away/recent state uses the amber
 * mention hue. The pulse halts under reduced motion (the keyframe is neutralized by the
 * global `prefers-reduced-motion` rule in index.css).
 */
export function PresenceDot({
  status = "offline",
  size = "md",
  className,
  label,
}: PresenceDotProps) {
  const px = SIZE[size];
  const color = COLOR[status];
  const online = status === "online";
  return (
    <span
      role="status"
      aria-label={label ?? status}
      className={cn("relative inline-flex shrink-0", className)}
      style={{ width: px, height: px }}
    >
      {online && (
        <span
          aria-hidden
          className="absolute inset-0 rounded-full"
          style={{
            backgroundColor: color,
            animation: "presence-breathe 2.4s ease-in-out infinite",
          }}
        />
      )}
      <span
        aria-hidden
        className="relative inline-block rounded-full ring-2 ring-background"
        style={{
          width: px,
          height: px,
          backgroundColor: color,
          opacity: status === "offline" ? 0.6 : 1,
        }}
      />
    </span>
  );
}
