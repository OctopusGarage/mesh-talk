import { cn } from "@/lib/utils";

export type PresenceStatus = "online" | "away" | "recent" | "offline";

const COLOR: Record<PresenceStatus, string> = {
  online: "hsl(var(--presence-online))",
  away: "hsl(var(--presence-recent))",
  recent: "hsl(var(--presence-recent))",
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
 * PresenceDot — a stable status marker. Online keeps the same teal meaning across
 * palettes; offline is dimmed and recent uses the secondary status hue.
 */
export function PresenceDot({
  status = "offline",
  size = "md",
  className,
  label,
}: PresenceDotProps) {
  const px = SIZE[size];
  const color = COLOR[status];
  return (
    <span
      role="status"
      aria-label={label ?? status}
      className={cn("relative inline-flex shrink-0", className)}
      style={{ width: px, height: px }}
    >
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
