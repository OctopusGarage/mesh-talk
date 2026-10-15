import { chat } from "@/lib/api/chat";
import { contactPolicy, favorites } from "@/lib/api";
import { useChat } from "@/store/chat";
import { useContactPolicy } from "@/store/contactPolicy";

function favoriteValue(record: {
  pinned: boolean;
  custom_alias: string | null;
  hidden?: boolean;
}): string {
  return JSON.stringify({
    pinned: record.pinned,
    custom_alias: record.custom_alias,
    hidden: record.hidden === true,
  });
}

async function localValues(owner: string): Promise<Map<string, string>> {
  const [pinned, hidden] = await Promise.all([
    favorites.get(owner),
    contactPolicy.get(owner),
  ]);
  const values = new Map<string, string>();
  for (const record of pinned)
    values.set(`favorite:${record.id}`, favoriteValue(record));
  for (const record of hidden.contacts)
    values.set(`hidden:${record.account_id}`, JSON.stringify(record.name));
  return values;
}

async function applyContactPreference(
  owner: string,
  key: string,
  value: string | null,
): Promise<void> {
  if (key.startsWith("favorite:")) {
    const id = key.slice("favorite:".length);
    if (!/^[a-f0-9]{32}$|^[a-f0-9]{64}$/.test(id)) return;
    let parsed: unknown = null;
    try {
      if (value !== null) parsed = JSON.parse(value);
    } catch {
      return;
    }
    const record =
      parsed && typeof parsed === "object"
        ? (parsed as Record<string, unknown>)
        : {};
    const pinned = record.pinned === true;
    const alias =
      typeof record.custom_alias === "string" &&
      record.custom_alias.length <= 256
        ? record.custom_alias
        : null;
    const hidden = record.hidden === true;
    await favorites.setFavorite(owner, id, pinned);
    await favorites.setAlias(owner, id, alias);
    await favorites.setChannelHidden(owner, id, hidden);
    await useChat.getState().loadFavorites();
  } else if (key.startsWith("hidden:")) {
    const account = key.slice("hidden:".length);
    if (!/^[a-f0-9]{32}$/.test(account)) return;
    let name = account.slice(0, 8);
    if (value !== null) {
      try {
        const parsed: unknown = JSON.parse(value);
        if (typeof parsed === "string" && parsed.length <= 1024) name = parsed;
      } catch {
        return;
      }
    }
    await contactPolicy.set(owner, account, value !== null, name);
    await useContactPolicy.getState().load(owner);
  }
}

/** Sync local pin/alias/visibility choices through the account preference log. */
export function startLinkedContacts(owner: string): () => void {
  let active = true;
  let polling = false;
  let initialized = false;
  let enabled = false;
  const baseline = new Map<string, string>();
  const revisions = new Map<string, number>();
  const pending = new Set<string>();
  const dirty = new Map<string, string | null>();
  const publish = (key: string, value: string | null) => {
    dirty.set(key, value);
    if (!enabled || pending.has(key)) return;
    pending.add(key);
    void chat
      .setAccountPreference("contacts", key, value)
      .then((record) => {
        if (active) {
          revisions.set(
            key,
            Math.max(revisions.get(key) ?? 0, record.revision),
          );
          if (dirty.get(key) === value) dirty.delete(key);
        }
      })
      .catch((error) =>
        console.warn("Could not save linked contact preference", error),
      )
      .finally(() => {
        pending.delete(key);
        if (active && dirty.has(key) && dirty.get(key) !== value)
          publish(key, dirty.get(key)!);
      });
  };
  const reconcile = async () => {
    if (!active || polling) return;
    polling = true;
    try {
      const [policy, remote, local] = await Promise.all([
        chat.getDeviceSyncPolicy(),
        chat.getAccountPreferences("contacts"),
        localValues(owner),
      ]);
      if (!active) return;
      enabled = policy.contacts;
      const remoteByKey = new Map(remote.map((record) => [record.key, record]));
      if (!initialized) {
        for (const [key, value] of local) {
          if (!remoteByKey.has(key)) publish(key, value);
        }
        for (const [key, value] of local) baseline.set(key, value);
        initialized = true;
      } else {
        for (const key of new Set([...baseline.keys(), ...local.keys()])) {
          const before = baseline.get(key) ?? null;
          const after = local.get(key) ?? null;
          if (before !== after) {
            baseline.delete(key);
            if (after !== null) baseline.set(key, after);
            publish(key, after);
          }
        }
      }
      if (enabled) {
        for (const [key, value] of dirty) publish(key, value);
      }
      if (policy.contacts) {
        for (const record of remote) {
          if (
            !active ||
            pending.has(record.key) ||
            dirty.has(record.key) ||
            record.revision <= (revisions.get(record.key) ?? 0)
          )
            continue;
          await applyContactPreference(owner, record.key, record.value);
          revisions.set(record.key, record.revision);
          if (record.value === null) baseline.delete(record.key);
          else baseline.set(record.key, record.value);
        }
      }
    } catch (error) {
      if (import.meta.env.PROD)
        console.warn("Could not reconcile linked contacts", error);
    } finally {
      polling = false;
    }
  };
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), 10_000);
  return () => {
    active = false;
    window.clearInterval(timer);
  };
}
