import i18n from "./i18n";

const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();
function relativeTime() {
  const language = i18n.resolvedLanguage ?? "en";
  let format = relativeFormats.get(language);
  if (!format) {
    format = new Intl.RelativeTimeFormat(language, {
      numeric: "auto",
      style: "narrow",
    });
    relativeFormats.set(language, format);
  }
  return format;
}

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
  return d.toLocaleTimeString(i18n.resolvedLanguage, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatDay(wall: number): string {
  const d = new Date(toMillis(wall));
  const today = new Date();
  const yest = new Date();
  yest.setDate(today.getDate() - 1);
  const offset =
    d.toDateString() === today.toDateString()
      ? 0
      : d.toDateString() === yest.toDateString()
        ? -1
        : null;
  if (offset !== null) {
    const label = relativeTime().format(offset, "day");
    return label.replace(/^./u, (initial) =>
      initial.toLocaleUpperCase(i18n.resolvedLanguage),
    );
  }
  return d.toLocaleDateString(i18n.resolvedLanguage, {
    month: "short",
    day: "numeric",
  });
}

/** Compact "Ns ago" / "Nm ago" for a whole-seconds elapsed value. */
export function formatAgo(secs: number): string {
  if (secs < 1) {
    return i18n.resolvedLanguage?.startsWith("en")
      ? "just now"
      : relativeTime().format(0, "second");
  }
  const [value, unit] =
    secs < 60
      ? ([Math.floor(secs), "second"] as const)
      : secs < 3600
        ? ([Math.floor(secs / 60), "minute"] as const)
        : ([Math.floor(secs / 3600), "hour"] as const);
  return relativeTime().format(-value, unit);
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
