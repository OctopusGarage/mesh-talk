import { chat } from "@/lib/api/chat";
import { appearanceValues, applyLinkedAppearance, useTheme } from "@/lib/theme";

/** Preserve the existing device's saved look before it issues a pairing code. */
export async function publishAppearanceBeforeLink(): Promise<void> {
  const existing = new Set(
    (await chat.getAccountPreferences("appearance")).map(
      (record) => record.key,
    ),
  );
  for (const [key, value] of Object.entries(appearanceValues())) {
    if (!existing.has(key))
      await chat.setAccountPreference("appearance", key, value);
  }
}

/** Reconcile the signed-in account's appearance with the local theme store. */
export function startLinkedAppearance(): () => void {
  let active = true;
  let polling = false;
  let applying = false;
  let enabled = false;
  const revisions = new Map<string, number>();
  const pending = new Set<string>();
  const dirty = new Map<string, string>();
  let previous = appearanceValues();
  const publish = (key: string, value: string) => {
    dirty.set(key, value);
    if (!enabled || pending.has(key)) return;
    pending.add(key);
    void chat
      .setAccountPreference("appearance", key, value)
      .then((record) => {
        if (active) {
          revisions.set(
            key,
            Math.max(revisions.get(key) ?? 0, record.revision),
          );
          if (dirty.get(key) === value) dirty.delete(key);
        }
      })
      .catch((error) => {
        console.warn("Could not save linked appearance", error);
      })
      .finally(() => {
        pending.delete(key);
        if (active && dirty.has(key) && dirty.get(key) !== value)
          publish(key, dirty.get(key)!);
      });
  };
  const unsubscribe = useTheme.subscribe(() => {
    const next = appearanceValues();
    if (active && !applying) {
      for (const [key, value] of Object.entries(next)) {
        if (previous[key] !== value) publish(key, value);
      }
    }
    previous = next;
  });
  const reconcile = async () => {
    if (polling || !active) return;
    polling = true;
    try {
      const [policy, records] = await Promise.all([
        chat.getDeviceSyncPolicy(),
        chat.getAccountPreferences("appearance"),
      ]);
      if (!active) return;
      enabled = policy.appearance;
      if (enabled) {
        for (const [key, value] of dirty) publish(key, value);
      }
      if (policy.appearance) {
        applying = true;
        try {
          for (const record of records) {
            if (
              !pending.has(record.key) &&
              !dirty.has(record.key) &&
              record.revision > (revisions.get(record.key) ?? 0)
            ) {
              applyLinkedAppearance(record.key, record.value);
              revisions.set(record.key, record.revision);
            }
          }
        } finally {
          applying = false;
          previous = appearanceValues();
        }
      }
    } catch (error) {
      // The browser preview has no native node; a later poll retries desktop failures.
      if (import.meta.env.PROD)
        console.warn("Could not reconcile linked appearance", error);
    } finally {
      polling = false;
    }
  };
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), 10_000);
  return () => {
    active = false;
    unsubscribe();
    window.clearInterval(timer);
  };
}
