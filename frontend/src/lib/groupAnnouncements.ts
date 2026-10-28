import type { ChatMessage } from "@/store/conversationModel";
import type { ChannelMetadataValue } from "./types";

const listeners = new Set<() => void>();
const sessionFallback = new Map<string, Record<string, string>>();
const storageKey = (owner: string, account: string) =>
  `mesh-talk-announcement-seen:${JSON.stringify([owner, account])}`;
function read(owner: string, account: string): Record<string, string> {
  const key = storageKey(owner, account);
  const fallback = sessionFallback.get(key);
  if (fallback) return fallback;
  try {
    const value: unknown = JSON.parse(localStorage.getItem(key) ?? "{}");
    if (value && typeof value === "object" && !Array.isArray(value))
      return Object.fromEntries(
        Object.entries(value)
          .filter(
            (entry): entry is [string, string] => typeof entry[1] === "string",
          )
          .slice(-256),
      );
  } catch {
    /* Private reminders still work for this session when storage is unavailable. */
  }
  return sessionFallback.get(key) ?? {};
}
/** Only version IDs are stored; no announcement text or public read receipt. */
export function announcementSeen(
  owner: string,
  account: string,
  channel: string,
): string {
  return read(owner, account)[channel] ?? "";
}
export function dismissAnnouncement(
  owner: string,
  account: string,
  channel: string,
  revision: string,
) {
  if (!owner || !account || !channel || !revision) return;
  const records = read(owner, account);
  delete records[channel];
  const bounded = Object.fromEntries([
    ...Object.entries(records).slice(-255),
    [channel, revision],
  ]);
  const key = storageKey(owner, account);
  try {
    localStorage.setItem(key, JSON.stringify(bounded));
    sessionFallback.delete(key);
  } catch {
    if (sessionFallback.size >= 16) sessionFallback.clear();
    sessionFallback.set(key, bounded);
  }
  for (const listener of listeners) listener();
}
export function subscribeAnnouncementSeen(listener: () => void) {
  listeners.add(listener);
  window.addEventListener("storage", listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("storage", listener);
  };
}
/** Presentation only: never insert publications into message delivery/unread ledgers. */
export function withAnnouncementPublications(
  history: ChatMessage[],
  publications: ChannelMetadataValue[],
  ownAccount: string,
): ChatMessage[] {
  const unique = new Map(publications.map((entry) => [entry.revision, entry]));
  if (!unique.size) return history;
  const notices: ChatMessage[] = [...unique.values()]
    .map((entry) => ({
      id: `announcement:${entry.revision}`,
      fromMe: entry.author === ownAccount,
      who: entry.author,
      text: "",
      wallClock: entry.authored_at,
      replyTo: null,
      announcement: entry,
    }))
    .sort(
      (a, b) =>
        a.wallClock - b.wallClock || (a.id ?? "").localeCompare(b.id ?? ""),
    );
  const merged: ChatMessage[] = [];
  let cursor = 0;
  for (const message of history) {
    while (
      cursor < notices.length &&
      notices[cursor].wallClock <= message.wallClock
    )
      merged.push(notices[cursor++]);
    merged.push(message);
  }
  return [...merged, ...notices.slice(cursor)];
}
