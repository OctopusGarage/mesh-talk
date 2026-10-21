import { readWithDeadline } from "./readDeadline";
import { subscribeSyncInvalidation } from "./syncInvalidation";
import { chat } from "@/lib/api/chat";
import {
  appearanceValues,
  savedAppearanceValues,
  applyLinkedAppearance,
  useTheme,
} from "@/lib/theme";
import { createLinkedPreferencePublisher } from "./linkedPreferencePublisher";

/** Preserve the existing device's saved look before it issues a pairing code. */
export async function publishAppearanceBeforeLink(
  owner: string,
): Promise<void> {
  const existing = new Set(
    (await chat.getAccountPreferences("appearance", owner)).map(
      (record) => record.key,
    ),
  );
  for (const [key, value] of Object.entries(savedAppearanceValues())) {
    if (!existing.has(key))
      await chat.setAccountPreference("appearance", key, value, owner);
  }
}

/** Reconcile the signed-in account's appearance with the local theme store. */
export function startLinkedAppearance(owner: string): () => void {
  let active = true;
  let polling = false;
  let dirty = false;
  let applying = false;
  const publisher = createLinkedPreferencePublisher(
    (key, value) => chat.setAccountPreference("appearance", key, value, owner),
    (error) => console.warn("Could not save linked appearance", error),
  );
  let previous = appearanceValues();
  const unsubscribe = useTheme.subscribe(() => {
    const next = appearanceValues();
    if (active && !applying) {
      for (const [key, value] of Object.entries(next)) {
        if (previous[key] !== value) publisher.publish(key, value);
      }
    }
    previous = next;
  });
  const reconcile = async () => {
    if (!active) return;
    if (polling) {
      dirty = true;
      return;
    }
    polling = true;
    try {
      const [policy, records] = await Promise.all([
        readWithDeadline(() => chat.getDeviceSyncPolicy(owner)),
        readWithDeadline(() => chat.getAccountPreferences("appearance", owner)),
      ]);
      if (!active) return;
      publisher.setEnabled(policy.appearance);
      if (policy.appearance) {
        applying = true;
        try {
          for (const record of records) {
            if (
              publisher.shouldApply(
                record.key,
                record.revision,
                record.device_id,
              )
            ) {
              applyLinkedAppearance(record.key, record.value);
              publisher.markApplied(
                record.key,
                record.revision,
                record.device_id,
              );
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
      if (active && dirty) {
        dirty = false;
        queueMicrotask(() => void reconcile());
      }
    }
  };
  const unlisten = subscribeSyncInvalidation((change) => {
    if (change.owner === owner && change.kind === "state") void reconcile();
  });
  void reconcile();
  const timer = window.setInterval(() => void reconcile(), 10_000);
  return () => {
    active = false;
    publisher.stop();
    unsubscribe();
    unlisten();
    window.clearInterval(timer);
  };
}
