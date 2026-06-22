/** First n hex chars of an id, for compact display. */
export function shortId(id: string, n = 8): string {
  return id.length > n ? id.slice(0, n) : id;
}

/** Backend timestamps may be seconds or millis; normalize to millis. */
function toMillis(wall: number): number {
  return wall < 1e12 ? wall * 1000 : wall;
}

export function formatTime(wall: number): string {
  const d = new Date(toMillis(wall));
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function formatDay(wall: number): string {
  const d = new Date(toMillis(wall));
  const today = new Date();
  const yest = new Date();
  yest.setDate(today.getDate() - 1);
  if (d.toDateString() === today.toDateString()) return "Today";
  if (d.toDateString() === yest.toDateString()) return "Yesterday";
  return d.toLocaleDateString([], { month: "short", day: "numeric" });
}

/** Compact "Ns ago" / "Nm ago" for a whole-seconds elapsed value. */
export function formatAgo(secs: number): string {
  if (secs < 1) return "just now";
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

export function humanSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.ceil(bytes / 1024)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

/**
 * Format `host:port`, bracketing the host if it is a bare (unbracketed) IPv6 literal,
 * so a v6 address doesn't render as the ambiguous `::1:80`. IPv4 and hostnames are
 * left as-is. Mirrors the Rust `util::address::format_host_port`.
 */
export function formatHostPort(host: string, port: number): string {
  const needsBrackets = host.includes(":") && !host.startsWith("[");
  return needsBrackets ? `[${host}]:${port}` : `${host}:${port}`;
}
