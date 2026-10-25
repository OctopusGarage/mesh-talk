import { captureRuntimeOwner } from "@/store/ownership";
import { readWithDeadline } from "./readDeadline";
import { subscribeSyncInvalidation } from "./syncInvalidation";
import type { FavoriteInfo, HiddenContactsSnapshot } from "./types";
import { chat } from "@/lib/api/chat";
import { contactPolicy, favorites } from "@/lib/api";
import { useChat } from "@/store/chat";
import { useContactPolicy } from "@/store/contactPolicy";
import { createLinkedPreferencePublisher } from "./linkedPreferencePublisher";

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

async function localValues(owner: string): Promise<{
  values: Map<string, string>;
  deleted: Set<string>;
}> {
  const [pinned, hidden] = await Promise.all([
    readWithDeadline(() => favorites.get(owner)),
    readWithDeadline(() => contactPolicy.get(owner)),
  ]);
  const values = new Map<string, string>();
  for (const record of pinned)
    values.set(`favorite:${record.id}`, favoriteValue(record));
  for (const record of hidden.contacts)
    values.set(`hidden:${record.account_id}`, JSON.stringify(record.name));
  return { values, deleted: new Set(hidden.deleted ?? []) };
}

/** Preserve saved source-device choices before exposing its pairing code. */
export async function publishContactsBeforeLink(owner: string): Promise<void> {
  const lease = captureRuntimeOwner();
  const accountId = useChat.getState().myAccountId;
  if (!accountId) throw new Error("Account identity is not ready");
  const current = () =>
    lease.owner === owner &&
    lease.current() &&
    useChat.getState().myAccountId === accountId;
  const [records, snapshot] = await Promise.all([
    readWithDeadline(() => chat.getAccountPreferences("contacts", owner)),
    localValues(owner),
  ]);
  if (!current()) throw new Error("Account changed during contact preparation");
  const existing = new Map(records.map((record) => [record.key, record]));
  for (const [key, value] of snapshot.values) {
    // Seeding is not an edit: preserve canonical values and tombstones. The
    // native absent-only operation closes the snapshot-to-write arrival race.
    if (!existing.has(key)) {
      if (!current())
        throw new Error("Account changed during contact preparation");
      await chat.seedAccountPreference(
        "contacts",
        key,
        value,
        owner,
        accountId,
      );
    }
  }
  if (!current()) throw new Error("Account changed during contact preparation");
}

function parseContactPreference(
  key: string,
  value: string | null,
): {
  favorite?: FavoriteInfo;
  hidden?: { account: string; hidden: boolean; name: string };
} {
  if (key.startsWith("favorite:")) {
    const id = key.slice("favorite:".length);
    if (!/^[a-f0-9]{32}$|^[a-f0-9]{64}$/.test(id)) return {};
    let parsed: unknown = null;
    try {
      if (value !== null) parsed = JSON.parse(value);
    } catch {
      return {};
    }
    if (
      value !== null &&
      (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
    )
      return {};
    const record = (parsed ?? {}) as Record<string, unknown>;
    if (
      ["pinned", "hidden"].some(
        (field) =>
          record[field] !== undefined && typeof record[field] !== "boolean",
      )
    )
      return {};
    if (
      record.custom_alias !== undefined &&
      record.custom_alias !== null &&
      (typeof record.custom_alias !== "string" ||
        Array.from(record.custom_alias).length > 256 ||
        new TextEncoder().encode(record.custom_alias).length > 1024)
    )
      return {};
    return {
      favorite: {
        id,
        pinned: record.pinned === true,
        custom_alias:
          typeof record.custom_alias === "string"
            ? record.custom_alias.trim() || null
            : null,
        hidden: record.hidden === true,
      },
    };
  }
  if (key.startsWith("hidden:")) {
    const account = key.slice("hidden:".length);
    if (!/^[a-f0-9]{32}$/.test(account)) return {};
    let name = account.slice(0, 8);
    if (value !== null) {
      try {
        const parsed: unknown = JSON.parse(value);
        if (typeof parsed === "string" && parsed.length <= 1024) name = parsed;
      } catch {
        return {};
      }
    }
    return { hidden: { account, hidden: value !== null, name } };
  }
  return {};
}

/** Sync local pin/alias/visibility choices through the account preference log. */
export function startLinkedContacts(owner: string): () => void {
  let active = true;
  const lease = captureRuntimeOwner();
  const accountId = useChat.getState().myAccountId;
  const current = () =>
    active &&
    lease.owner === owner &&
    lease.current() &&
    Boolean(accountId) &&
    useChat.getState().myAccountId === accountId;
  let polling = false;
  let dirty = false;
  let initialized = false;
  const baseline = new Map<string, string>();
  const publisher = createLinkedPreferencePublisher(
    (key, value) => {
      if (!current())
        return Promise.reject(
          new Error("Account changed during contact synchronization"),
        );
      return chat.setAccountPreference(
        "contacts",
        key,
        value,
        owner,
        accountId,
      );
    },
    (error) => console.warn("Could not save linked contact preference", error),
  );
  const reconcile = async () => {
    if (!current()) return;
    if (polling) {
      dirty = true;
      return;
    }
    polling = true;
    try {
      const [policy, remote, snapshot] = await Promise.all([
        readWithDeadline(() => chat.getDeviceSyncPolicy(owner)),
        readWithDeadline(() => chat.getAccountPreferences("contacts", owner)),
        localValues(owner),
      ]);
      if (!current()) return;
      const local = snapshot.values;
      publisher.setEnabled(policy.contacts);
      if (!initialized) {
        // A joining installation may still retain another account's local
        // choices. Only later committed edits are outgoing; explicit source
        // preparation is the sole migration seed.
        for (const [key, value] of local) baseline.set(key, value);
        initialized = true;
      } else {
        for (const key of new Set([...baseline.keys(), ...local.keys()])) {
          const before = baseline.get(key) ?? null;
          const after = local.get(key) ?? null;
          if (before !== after) {
            baseline.delete(key);
            if (after !== null) baseline.set(key, after);
            publisher.publish(key, after);
          }
        }
      }
      if (policy.contacts) {
        const selected = remote.filter(
          (record) =>
            !(
              record.key.startsWith("hidden:") &&
              snapshot.deleted.has(record.key.slice("hidden:".length))
            ) &&
            publisher.shouldApply(
              record.key,
              record.revision,
              record.device_id,
            ),
        );
        const parsed = selected.map((record) =>
          parseContactPreference(record.key, record.value),
        );
        const pinned = parsed.flatMap((record) =>
          record.favorite ? [record.favorite] : [],
        );
        const hidden = parsed.flatMap((record) =>
          record.hidden ? [record.hidden] : [],
        );
        if (pinned.length)
          await favorites.apply(
            owner,
            accountId,
            pinned,
            Object.fromEntries(
              pinned.map((record) => {
                const previous = local.get(`favorite:${record.id}`);
                return [
                  record.id,
                  previous === undefined ? null : JSON.parse(previous),
                ];
              }),
            ),
          );
        if (!current()) return;
        let committedHidden: HiddenContactsSnapshot | undefined;
        if (hidden.length)
          committedHidden = await contactPolicy.apply(
            owner,
            accountId,
            hidden,
            Object.fromEntries(
              hidden.map((record) => {
                const previous = local.get(`hidden:${record.account}`);
                return [
                  record.account,
                  previous === undefined ? null : JSON.parse(previous),
                ];
              }),
            ),
          );
        if (!current()) return;
        // Publish complete committed projections once per imported batch.
        await Promise.all([
          pinned.length ? useChat.getState().loadFavorites() : undefined,
          hidden.length ? useContactPolicy.getState().load(owner) : undefined,
        ]);
        if (!current()) return;
        for (const [index, record] of selected.entries()) {
          publisher.markApplied(record.key, record.revision, record.device_id);
          // Unsupported records remain owned by the publishing client. They are
          // not local deletions and must never become outgoing tombstones.
          if (!parsed[index].favorite && !parsed[index].hidden) continue;
          const favorite = parsed[index].favorite;
          const hidden = parsed[index].hidden;
          // Compare future local snapshots with the committed representation,
          // not the wire JSON. Optional fields, trimmed aliases and pruned empty
          // rows must not become false local edits that overwrite newer peers.
          if (favorite) {
            if (!favorite.pinned && !favorite.custom_alias && !favorite.hidden)
              baseline.delete(record.key);
            else baseline.set(record.key, favoriteValue(favorite));
          } else if (hidden) {
            const committed = committedHidden?.contacts.find(
              (entry) => entry.account_id === hidden.account,
            );
            if (committed)
              baseline.set(record.key, JSON.stringify(committed.name));
            else baseline.delete(record.key);
          }
        }
      }
    } catch (error) {
      if (import.meta.env.PROD)
        console.warn("Could not reconcile linked contacts", error);
    } finally {
      polling = false;
      if (current() && dirty) {
        dirty = false;
        queueMicrotask(() => void reconcile());
      }
    }
  };
  const unlisten = subscribeSyncInvalidation((change) => {
    if (
      change.owner === owner &&
      (change.kind === "state" || change.kind === "contacts")
    )
      void reconcile();
  });
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), 10_000);
  return () => {
    active = false;
    publisher.stop();
    unlisten();
    window.clearInterval(timer);
  };
}
