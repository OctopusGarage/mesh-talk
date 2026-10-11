const PREFIX = "mesh-talk-unread-v1";
const MAX_SNAPSHOT_BYTES = 1_000_000;

function key(owner: string, accountId: string): string {
  return `${PREFIX}:${owner}:${accountId}`;
}

function validEntry(conversation: string, count: unknown): count is number {
  return (
    conversation.length <= 200 &&
    (conversation.startsWith("account:") ||
      conversation.startsWith("channel:")) &&
    Number.isSafeInteger(count) &&
    Number(count) > 0
  );
}

/** Persist only opaque conversation keys and counts, scoped to both host and mesh account. */
export function loadUnread(
  owner: string,
  accountId: string,
): Record<string, number> {
  try {
    const encoded = localStorage.getItem(key(owner, accountId));
    if (!encoded || encoded.length > MAX_SNAPSHOT_BYTES) return {};
    const parsed: unknown = JSON.parse(encoded);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
      return {};
    return Object.fromEntries(
      Object.entries(parsed).filter(([conversation, count]) =>
        validEntry(conversation, count),
      ),
    );
  } catch {
    return {};
  }
}

export function saveUnread(
  owner: string,
  accountId: string,
  unread: Record<string, number>,
): void {
  try {
    const entries = Object.entries(unread).filter(([conversation, count]) =>
      validEntry(conversation, count),
    );
    localStorage.setItem(
      key(owner, accountId),
      JSON.stringify(Object.fromEntries(entries)),
    );
  } catch {
    // Storage may be unavailable; the in-memory counters still work this session.
  }
}
